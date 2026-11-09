//! Importing, updating, switching and removing subscriptions (plan §7.1),
//! and the config file the kernel runs from the one in use (plan §7.2).

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use jiff::Timestamp;
use serde_json::json;

use super::{
    Action, CoreError, ManagedCore, encrypted,
    files::write_private,
    install,
    runner::Cmd,
    runtime,
    state::random_hex,
    subscription::{
        DEFAULT_INTERVAL_HOURS, Kind, Store, Subscription, Usage, first_error, link_count,
        parse_filename, parse_usage,
    },
};

/// What a list of share links is wrapped into: the group the nodes go in.
const GROUP: &str = "节点选择";

/// What an operation on the subscriptions did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The list as it is now.
    pub store: Store,
    /// The kernel's config file changed: it has to read it again.
    pub reload: bool,
    /// The subscription it was about.
    pub name: String,
}

/// A subscription as its source delivered it.
struct Fetched {
    /// What it said: text, or an age file.
    bytes: Vec<u8>,
    /// What the server calls it.
    name: Option<String>,
    usage: Option<Usage>,
}

/// A subscription ready to be put in place.
struct Prepared {
    /// What the kernel is run with: the subscription itself, or what wraps
    /// its share links.
    profile: String,
    /// The share links, which the wrapper's provider reads.
    links: Option<String>,
}

fn now() -> i64 {
    Timestamp::now().as_second()
}

impl ManagedCore {
    /// The subscriptions imported so far.
    pub fn subscriptions(&self) -> Result<Store, CoreError> {
        Store::load(&self.paths.subscriptions())
            .map_err(|err| CoreError::io("cannot read subscriptions.toml", err))
    }

    fn save_subscriptions(&self, store: &Store) -> Result<(), CoreError> {
        store
            .save(&self.paths.subscriptions())
            .map_err(|err| CoreError::io("cannot write subscriptions.toml", err))
    }

    /// The profile the kernel runs: the subscription in use, or the one that
    /// sends everything out directly when there is none.
    fn base_profile(&self, store: &Store) -> Result<String, CoreError> {
        match store.active() {
            Some(sub) => fs::read_to_string(self.paths.profile(&sub.id))
                .map_err(|err| CoreError::io("cannot read the subscription in use", err)),
            None => Ok(runtime::BUILTIN.to_owned()),
        }
    }

    /// Writes the kernel's config file from the subscription `store` says is
    /// in use.
    pub(super) fn write_config_for(&self, store: &Store) -> Result<(), CoreError> {
        let text = runtime::render(&self.base_profile(store)?, &self.settings(), &self.state)
            .map_err(|_| CoreError::UnsupportedProfile)?;
        write_private(&self.paths.config(), &text)
            .map_err(|err| CoreError::io("cannot write the kernel's config", err))
    }

    /// Downloads the subscription at `url`.
    async fn fetch(&self, url: &str) -> Result<Fetched, CoreError> {
        let failed = |err: reqwest::Error| CoreError::Fetch(install::describe(&err.without_url()));
        let response = self.http.get(url).send().await.map_err(failed)?;
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Fetch(format!("HTTP {status}")));
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let name = header("content-disposition").and_then(|value| parse_filename(&value));
        let usage = header("subscription-userinfo").and_then(|value| parse_usage(&value));
        let bytes = response.bytes().await.map_err(failed)?.to_vec();
        Ok(Fetched { bytes, name, usage })
    }

    /// Runs the kernel's own check on the config in `file`: `None` when it
    /// passes, else what it complains about.
    async fn check(&self, file: &Path) -> Result<Option<String>, CoreError> {
        let binary = self.binary()?;
        let output = self
            .exec(Cmd::new(
                &binary.to_string_lossy(),
                [
                    "-t".to_owned(),
                    "-d".to_owned(),
                    self.paths.home().to_string_lossy().into_owned(),
                    "-f".to_owned(),
                    file.to_string_lossy().into_owned(),
                ],
            ))
            .await
            .map_err(|err| CoreError::Command {
                action: Action::Validate,
                message: err.to_string(),
            })?;
        if output.status.success() {
            return Ok(None);
        }
        let mut said = String::from_utf8_lossy(&output.stdout).into_owned();
        said.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok(Some(first_error(&said)))
    }

    /// [`ManagedCore::check`] of `text`, which is put in a file for it.
    async fn check_text(&self, id: &str, text: &str) -> Result<Option<String>, CoreError> {
        let candidate = self.paths.profiles().join(format!("{id}.check.yaml"));
        write_private(&candidate, text)
            .map_err(|err| CoreError::io("cannot write the subscription", err))?;
        let result = self.check(&candidate).await;
        let _ = fs::remove_file(&candidate);
        result
    }

    /// What the kernel is run with for a subscription that is a list of
    /// share links: a provider that reads them, and a group to pick a node
    /// from (plan §7.1). The kernel only reads a provider's file from inside
    /// its home directory.
    fn wrapper(&self, id: &str, name: &str) -> String {
        let provider = json!({
            "type": "file",
            "path": format!("./providers/{id}.txt"),
            "health-check": { "enable": true, "url": self.delay_url, "interval": 600 },
        });
        let group = json!({ "name": GROUP, "type": "select", "use": [name] });
        format!(
            "proxy-providers: {}\nproxy-groups: [{group}]\nrules: [{}]\n",
            json!({ (name): provider }),
            json!(format!("MATCH,{GROUP}")),
        )
    }

    /// Checks `text` as the kernel would have to read it, and decides what
    /// of it is the profile (plan §7.1): a config the kernel accepts is the
    /// profile; failing that, a list of share links is read through a
    /// wrapper. `mihomo -t` does not open provider files, so what is a list
    /// of links is decided here. Anything else is refused with the kernel's
    /// own words.
    async fn prepare(&self, id: &str, name: &str, text: &str) -> Result<Prepared, CoreError> {
        let Some(complaint) = self.check_text(id, text).await? else {
            return self.accept(text.to_owned(), None);
        };
        if link_count(text) == 0 {
            return Err(CoreError::Invalid(complaint));
        }
        let wrapper = self.wrapper(id, name);
        if let Some(complaint) = self.check_text(id, &wrapper).await? {
            return Err(CoreError::Invalid(complaint));
        }
        self.accept(wrapper, Some(text.to_owned()))
    }

    /// The profile, if the config file can be made from it.
    fn accept(&self, profile: String, links: Option<String>) -> Result<Prepared, CoreError> {
        runtime::render(&profile, &self.settings(), &self.state)
            .map_err(|_| CoreError::UnsupportedProfile)?;
        Ok(Prepared { profile, links })
    }

    /// Puts a prepared subscription in place of the one of that id, each
    /// file whole or not at all.
    fn commit(&self, id: &str, prepared: &Prepared) -> Result<(), CoreError> {
        let io = |err| CoreError::io("cannot write the subscription", err);
        let provider = self.paths.provider(id);
        match &prepared.links {
            Some(links) => write_private(&provider, links).map_err(io)?,
            // It was a list of links, and is not any more.
            None => {
                let _ = fs::remove_file(&provider);
            }
        }
        write_private(&self.paths.profile(id), &prepared.profile).map_err(io)
    }

    /// Adds the subscription `fetched` to the list, and makes it the one in
    /// use when there is none.
    async fn add(
        &self,
        name: String,
        kind: Kind,
        source: String,
        fetched: Fetched,
        age_key: Option<&str>,
    ) -> Result<Outcome, CoreError> {
        let id = random_hex(4).map_err(|err| CoreError::io("cannot make an id", err))?;
        // Before anything is written: a key that does not open it leaves
        // nothing behind.
        let (text, age_key) = plain(&fetched.bytes, age_key)?;
        let prepared = self.prepare(&id, &name, &text).await?;
        self.commit(&id, &prepared)?;
        let mut store = self.subscriptions()?;
        store.subscriptions.push(Subscription {
            id: id.clone(),
            name: name.clone(),
            kind,
            source,
            auto_update: true,
            interval_hours: DEFAULT_INTERVAL_HOURS,
            updated_at: now(),
            usage: fetched.usage,
            age_key,
        });
        let reload = store.active.is_none();
        if reload {
            store.active = Some(id);
            self.write_config_for(&store)?;
        }
        self.save_subscriptions(&store)?;
        Ok(Outcome {
            store,
            reload,
            name,
        })
    }

    /// Imports the subscription at `url`; `age_key` is what opens it when
    /// it is encrypted with age.
    pub async fn import_url(&self, url: &str, age_key: Option<&str>) -> Result<Outcome, CoreError> {
        let parsed = reqwest::Url::parse(url)
            .ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https"))
            .ok_or_else(|| CoreError::Fetch("not an http or https address".to_owned()))?;
        let _guard = self.lock.lock().await;
        let fetched = self.fetch(url).await?;
        let name = fetched
            .name
            .clone()
            .or_else(|| parsed.host_str().map(str::to_owned))
            .unwrap_or_else(|| "subscription".to_owned());
        self.add(name, Kind::Url, url.to_owned(), fetched, age_key)
            .await
    }

    /// Imports the subscription in the file at `path`; `age_key` is what
    /// opens it when it is encrypted with age.
    pub async fn import_file(
        &self,
        path: &Path,
        age_key: Option<&str>,
    ) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let bytes = read_source(&path)?;
        let name = path.file_stem().map_or_else(
            || "subscription".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        let fetched = Fetched {
            bytes,
            name: None,
            usage: None,
        };
        self.add(
            name,
            Kind::File,
            path.to_string_lossy().into_owned(),
            fetched,
            age_key,
        )
        .await
    }

    /// Reads subscription `id` again from where it came from. What was
    /// there stays when that fails.
    pub async fn update(&self, id: &str) -> Result<Outcome, CoreError> {
        self.update_with(id, None).await
    }

    /// Reads subscription `id` again, with `key` to open it in place of the
    /// one kept with it; the key is kept when it does.
    pub async fn set_age_key(&self, id: &str, key: &str) -> Result<Outcome, CoreError> {
        self.update_with(id, Some(key)).await
    }

    async fn update_with(&self, id: &str, key: Option<&str>) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let mut store = self.subscriptions()?;
        let sub = store.get(id).cloned().ok_or(CoreError::NoSubscription)?;
        let fetched = match sub.kind {
            Kind::Url => self.fetch(&sub.source).await?,
            Kind::File => Fetched {
                bytes: read_source(Path::new(&sub.source))?,
                name: None,
                usage: None,
            },
        };
        let (text, age_key) = plain(&fetched.bytes, key.or(sub.age_key.as_deref()))?;
        let prepared = self.prepare(id, &sub.name, &text).await?;
        self.commit(id, &prepared)?;
        if let Some(entry) = store.get_mut(id) {
            entry.updated_at = now();
            // No longer encrypted: the key has nothing to open.
            entry.age_key = age_key;
            if sub.kind == Kind::Url {
                entry.usage = fetched.usage;
            }
        }
        let reload = store.active.as_deref() == Some(id);
        if reload {
            self.write_config_for(&store)?;
        }
        self.save_subscriptions(&store)?;
        Ok(Outcome {
            store,
            reload,
            name: sub.name,
        })
    }

    /// Makes subscription `id` the one the kernel runs.
    pub async fn switch(&self, id: &str) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let mut store = self.subscriptions()?;
        let name = store
            .get(id)
            .map(|sub| sub.name.clone())
            .ok_or(CoreError::NoSubscription)?;
        if store.active.as_deref() == Some(id) {
            return Ok(Outcome {
                store,
                reload: false,
                name,
            });
        }
        store.active = Some(id.to_owned());
        self.write_config_for(&store)?;
        self.save_subscriptions(&store)?;
        Ok(Outcome {
            store,
            reload: true,
            name,
        })
    }

    /// Gives subscription `id` another name. The name is only what the
    /// list shows: the files go by the id.
    pub async fn rename(&self, id: &str, name: &str) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let mut store = self.subscriptions()?;
        let entry = store.get_mut(id).ok_or(CoreError::NoSubscription)?;
        name.trim().clone_into(&mut entry.name);
        let name = entry.name.clone();
        self.save_subscriptions(&store)?;
        Ok(Outcome {
            store,
            reload: false,
            name,
        })
    }

    /// Sets how many hours pass between the automatic updates of
    /// subscription `id`.
    pub async fn set_interval(&self, id: &str, hours: u32) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let mut store = self.subscriptions()?;
        let entry = store.get_mut(id).ok_or(CoreError::NoSubscription)?;
        entry.interval_hours = hours;
        let name = entry.name.clone();
        self.save_subscriptions(&store)?;
        Ok(Outcome {
            store,
            reload: false,
            name,
        })
    }

    /// Turns the automatic update of subscription `id` on or off.
    pub async fn set_auto_update(&self, id: &str, on: bool) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let mut store = self.subscriptions()?;
        let entry = store.get_mut(id).ok_or(CoreError::NoSubscription)?;
        entry.auto_update = on;
        let name = entry.name.clone();
        self.save_subscriptions(&store)?;
        Ok(Outcome {
            store,
            reload: false,
            name,
        })
    }

    /// Deletes subscription `id` and its files. The one in use stays: the
    /// kernel is running it.
    pub async fn remove(&self, id: &str) -> Result<Outcome, CoreError> {
        let _guard = self.lock.lock().await;
        let mut store = self.subscriptions()?;
        let index = store
            .subscriptions
            .iter()
            .position(|sub| sub.id == id)
            .ok_or(CoreError::NoSubscription)?;
        if store.active.as_deref() == Some(id) {
            return Err(CoreError::ActiveSubscription);
        }
        let sub = store.subscriptions.remove(index);
        self.save_subscriptions(&store)?;
        for file in [self.paths.profile(id), self.paths.provider(id)] {
            if let Err(err) = fs::remove_file(&file)
                && err.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!("cannot remove {}: {err}", file.display());
            }
        }
        Ok(Outcome {
            store,
            reload: false,
            name: sub.name,
        })
    }
}

/// The text `bytes` hold, and the key that opened it: an age file is read
/// with `key` (as the user typed it), anything else is text as it is.
fn plain(bytes: &[u8], key: Option<&str>) -> Result<(String, Option<String>), CoreError> {
    if !encrypted::is_encrypted(bytes) {
        return Ok((String::from_utf8_lossy(bytes).into_owned(), None));
    }
    let key = encrypted::key_line(key.ok_or(CoreError::AgeKeyNeeded)?).map_err(CoreError::Age)?;
    let opened = encrypted::decrypt(bytes, &key).map_err(CoreError::Age)?;
    Ok((String::from_utf8_lossy(&opened).into_owned(), Some(key)))
}

/// What the file a subscription is read from holds; a file that is gone
/// is not an error of the subscription, which keeps its last copy.
fn read_source(path: &Path) -> Result<Vec<u8>, CoreError> {
    fs::read(path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            CoreError::SourceGone(path.to_owned())
        } else {
            CoreError::io("cannot read the subscription file", err)
        }
    })
}

/// The files of the managed core that hold subscriptions.
impl super::Paths {
    pub(super) fn subscriptions(&self) -> PathBuf {
        self.root.join("subscriptions.toml")
    }

    pub(super) fn profiles(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub(super) fn profile(&self, id: &str) -> PathBuf {
        self.profiles().join(format!("{id}.yaml"))
    }

    /// The share links of a subscription that is a list of them.
    pub(super) fn provider(&self, id: &str) -> PathBuf {
        self.home().join("providers").join(format!("{id}.txt"))
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::core::testing::{Recorder, answer, core_at, free_port, scratch};

    const SUB: &str = "proxies:\n  - {name: a, type: ss, server: 1.2.3.4, port: 1, cipher: aes-128-gcm, password: p}\nrules:\n  - MATCH,DIRECT\n";
    const OTHER: &str = "proxies:\n  - {name: b, type: ss, server: 5.6.7.8, port: 2, cipher: aes-128-gcm, password: q}\nrules:\n  - MATCH,DIRECT\n";
    const LINKS: &str = "ss://YWVzLTEyOC1nY206cA@1.2.3.4:1#a\ntrojan://pw@5.6.7.8:443#b\n";

    /// A kernel that checks like the real one: a config it can read passes,
    /// text that is not YAML fails with a log line, and a provider file is
    /// never opened.
    fn kernel() -> Arc<Recorder> {
        Recorder::new(|cmd| {
            if cmd.args.first().map(String::as_str) != Some("-t") {
                return answer(0, "");
            }
            let text = fs::read_to_string(cmd.args.last().unwrap()).unwrap();
            if text.contains("proxies") || text.contains("proxy-providers") {
                answer(0, "configuration file test is successful\n")
            } else {
                let mut output = answer(1, "");
                output.stdout =
                    b"time=\"t\" level=error msg=\"yaml: unmarshal errors: not a config\"\n"
                        .to_vec();
                output
            }
        })
    }

    fn core(name: &str) -> (Arc<ManagedCore>, Arc<Recorder>, PathBuf) {
        let dir = scratch(name);
        let recorder = kernel();
        let core = Arc::new(core_at(&dir, free_port(), &recorder, false));
        (core, recorder, dir)
    }

    async fn serve(body: &str, headers: &[(&str, &str)]) -> MockServer {
        let server = MockServer::start().await;
        let mut response = ResponseTemplate::new(200).set_body_string(body);
        for (name, value) in headers {
            response = response.insert_header(*name, *value);
        }
        Mock::given(method("GET"))
            .and(path("/sub"))
            .respond_with(response)
            .mount(&server)
            .await;
        server
    }

    fn url(server: &MockServer) -> String {
        format!("{}/sub?token=SECRET", server.uri())
    }

    fn config_of(dir: &Path) -> String {
        fs::read_to_string(dir.join("core/home/config.yaml")).unwrap()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[tokio::test]
    async fn an_imported_url_is_named_by_the_server_counted_and_the_first_one_runs() {
        let (core, recorder, dir) = core("sub-import");
        let server = serve(
            SUB,
            &[
                (
                    "content-disposition",
                    "attachment; filename*=UTF-8''%E6%9C%BA%E5%9C%BA.yaml",
                ),
                (
                    "subscription-userinfo",
                    "upload=1; download=2; total=30; expire=1700000000",
                ),
            ],
        )
        .await;

        let outcome = core.import_url(&url(&server), None).await.unwrap();

        assert!(outcome.reload && outcome.name == "机场");
        let sub = &outcome.store.subscriptions[0];
        assert_eq!(outcome.store.active.as_deref(), Some(sub.id.as_str()));
        assert_eq!(
            (sub.kind, sub.source.as_str()),
            (Kind::Url, url(&server).as_str())
        );
        assert!(sub.auto_update && sub.interval_hours == 24 && sub.updated_at > 0);
        assert_eq!(
            sub.usage,
            Some(Usage {
                upload: 1,
                download: 2,
                total: 30,
                expire: 1_700_000_000
            })
        );
        // The profile as it came, and the config made of it; both private.
        let profile = dir.join(format!("core/profiles/{}.yaml", sub.id));
        assert_eq!(fs::read_to_string(&profile).unwrap(), SUB);
        assert!(config_of(&dir).starts_with(SUB));
        for file in [
            &profile,
            &dir.join("core/subscriptions.toml"),
            &dir.join("core/home/config.yaml"),
        ] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Only the profile is kept: the file that was checked is gone.
        assert_eq!(
            files_in(&dir.join("core/profiles")),
            [format!("{}.yaml", sub.id)]
        );
        assert_eq!(core.subscriptions().unwrap(), outcome.store);
        // The server was asked as a client of the kind that gets a clash config.
        let requests = server.received_requests().await.unwrap();
        let agent = requests[0]
            .headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            agent.starts_with("tabby/v") && agent.contains("clash-verge"),
            "{agent}"
        );
        // The kernel was asked to check it, with its own home directory.
        let checks = recorder.runs(dir.join("mihomo").to_str().unwrap());
        assert_eq!(checks.len(), 1);
        assert!(checks[0].starts_with(&format!("-t -d {}", dir.join("core/home").display())));
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_second_import_does_not_take_the_place_of_the_one_running() {
        let (core, _, dir) = core("sub-second");
        let server = serve(SUB, &[]).await;
        let first = core.import_url(&url(&server), None).await.unwrap();
        // No name from the server: the host's.
        assert_eq!(first.name, "127.0.0.1");

        let second = core.import_url(&url(&server), None).await.unwrap();

        assert!(!second.reload);
        assert_eq!(second.store.subscriptions.len(), 2);
        assert_eq!(second.store.active, first.store.active);
        assert_ne!(
            second.store.subscriptions[0].id,
            second.store.subscriptions[1].id
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_list_of_share_links_is_read_through_a_provider_and_a_group() {
        let (core, recorder, dir) = core("sub-links");
        let encoded = STANDARD.encode(LINKS);
        let server = serve(
            &encoded,
            &[("content-disposition", "attachment; filename=\"My Sub\"")],
        )
        .await;

        let outcome = core.import_url(&url(&server), None).await.unwrap();

        let id = &outcome.store.subscriptions[0].id;
        // The links as they came, where the kernel may read them.
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/home/providers/{id}.txt"))).unwrap(),
            encoded
        );
        let profile = fs::read_to_string(dir.join(format!("core/profiles/{id}.yaml"))).unwrap();
        assert!(
            profile.contains(&format!("\"path\":\"./providers/{id}.txt\"")),
            "{profile}"
        );
        assert!(profile.contains("\"My Sub\":{"), "{profile}");
        assert!(profile.contains("\"use\":[\"My Sub\"]"), "{profile}");
        assert!(profile.contains("MATCH,节点选择"), "{profile}");
        assert!(profile.contains("generate_204"), "{profile}");
        assert!(config_of(&dir).starts_with(&profile));
        // The text was refused as a config first, the wrapper then accepted.
        assert_eq!(recorder.runs(dir.join("mihomo").to_str().unwrap()).len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn what_is_neither_a_config_nor_links_is_refused_with_the_kernels_words_and_leaves_nothing()
     {
        let (core, _, dir) = core("sub-garbage");
        // The kernel does not open provider files, so a missing or garbage one
        // would pass its check: tabby has to look itself.
        for body in ["<html>login page</html>", "aGVsbG8gd29ybGQ=", ""] {
            let server = serve(body, &[]).await;
            let err = core.import_url(&url(&server), None).await.unwrap_err();
            assert!(
                matches!(&err, CoreError::Invalid(said) if said == "yaml: unmarshal errors: not a config"),
                "{body:?}: {err:?}"
            );
        }
        assert_eq!(core.subscriptions().unwrap(), Store::default());
        assert_eq!(files_in(&dir.join("core/profiles")), Vec::<String>::new());
        assert!(!dir.join("core/home/providers").exists());
        assert!(!dir.join("core/home/config.yaml").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_profile_the_config_cannot_be_made_from_is_refused_whole() {
        let (core, _, dir) = core("sub-flow");
        // The kernel reads it; tabby cannot take the keys it sets out of it.
        let server = serve("{proxies: [], rules: [\"MATCH,DIRECT\"]}", &[]).await;

        let err = core.import_url(&url(&server), None).await.unwrap_err();

        assert!(matches!(err, CoreError::UnsupportedProfile), "{err:?}");
        assert_eq!(core.subscriptions().unwrap(), Store::default());
        assert_eq!(files_in(&dir.join("core/profiles")), Vec::<String>::new());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_download_that_fails_says_why_without_the_address_that_has_a_token_in_it() {
        let (core, _, dir) = core("sub-down");
        let refused = format!("http://127.0.0.1:{}/sub?token=SECRET", free_port());
        let err = core.import_url(&refused, None).await.unwrap_err();
        assert!(
            matches!(&err, CoreError::Fetch(message) if !message.contains("SECRET")),
            "{err:?}"
        );

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let err = core.import_url(&url(&server), None).await.unwrap_err();
        assert!(
            matches!(&err, CoreError::Fetch(message) if message.contains("503")),
            "{err:?}"
        );

        let err = core
            .import_url("ftp://example.test/sub", None)
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Fetch(_)), "{err:?}");
        assert_eq!(core.subscriptions().unwrap(), Store::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_update_replaces_the_content_and_reloads_only_what_runs() {
        let (core, _, dir) = core("sub-update");
        let server = serve(SUB, &[("subscription-userinfo", "download=5")]).await;
        let first = core.import_url(&url(&server), None).await.unwrap();
        let running = first.store.subscriptions[0].id.clone();
        let spare = core.import_file(&dir.join("mihomo"), None).await;
        // (The test kernel script is not a config.)
        assert!(matches!(spare, Err(CoreError::Invalid(_))));
        let other = dir.join("other.yaml");
        fs::write(&other, OTHER).unwrap();
        let spare = core.import_file(&other, None).await.unwrap();
        let spare_id = spare.store.subscriptions[1].id.clone();

        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/sub"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(OTHER)
                    .insert_header("subscription-userinfo", "download=9; total=10"),
            )
            .mount(&server)
            .await;
        let updated = core.update(&running).await.unwrap();

        assert!(updated.reload);
        assert!(config_of(&dir).starts_with(OTHER));
        let sub = updated.store.get(&running).unwrap();
        assert_eq!(
            sub.usage,
            Some(Usage {
                download: 9,
                total: 10,
                ..Usage::default()
            })
        );
        // A subscription that is not running is only written.
        fs::write(&other, SUB).unwrap();
        let not_running = core.update(&spare_id).await.unwrap();
        assert!(!not_running.reload);
        assert!(config_of(&dir).starts_with(OTHER));
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/profiles/{spare_id}.yaml"))).unwrap(),
            SUB
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_update_keeps_what_was_there() {
        let (core, _, dir) = core("sub-update-fail");
        let server = serve(SUB, &[]).await;
        let imported = core.import_url(&url(&server), None).await.unwrap();
        let id = imported.store.subscriptions[0].id.clone();
        let before = (config_of(&dir), core.subscriptions().unwrap());

        for (status, body) in [(500, "boom"), (200, "<html>maintenance</html>")] {
            server.reset().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            assert!(core.update(&id).await.is_err(), "{status}");
            assert_eq!(config_of(&dir), before.0);
            assert_eq!(core.subscriptions().unwrap(), before.1);
            assert_eq!(
                fs::read_to_string(dir.join(format!("core/profiles/{id}.yaml"))).unwrap(),
                SUB
            );
        }
        assert!(matches!(
            core.update("nope").await,
            Err(CoreError::NoSubscription)
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_file_that_is_gone_keeps_its_copy_and_a_file_that_became_links_gets_a_wrapper() {
        let (core, _, dir) = core("sub-file");
        let file = dir.join("mine.txt");
        fs::write(&file, SUB).unwrap();
        let imported = core.import_file(&file, None).await.unwrap();
        let id = imported.store.subscriptions[0].id.clone();
        assert_eq!(imported.name, "mine");
        assert_eq!(imported.store.subscriptions[0].kind, Kind::File);

        fs::write(&file, LINKS).unwrap();
        core.update(&id).await.unwrap();
        let provider = dir.join(format!("core/home/providers/{id}.txt"));
        assert_eq!(fs::read_to_string(&provider).unwrap(), LINKS);
        assert!(config_of(&dir).contains("proxy-providers"));

        // And back to a config: the links of the wrapper are not left behind.
        fs::write(&file, SUB).unwrap();
        core.update(&id).await.unwrap();
        assert!(!provider.exists());
        assert!(config_of(&dir).starts_with(SUB));

        fs::remove_file(&file).unwrap();
        let err = core.update(&id).await.unwrap_err();
        assert!(
            matches!(&err, CoreError::SourceGone(gone) if *gone == file),
            "{err:?}"
        );
        assert!(config_of(&dir).starts_with(SUB));
        assert!(dir.join(format!("core/profiles/{id}.yaml")).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn switching_regenerates_the_config_and_the_one_running_is_not_deleted() {
        let (core, _, dir) = core("sub-switch");
        let (a, b) = (dir.join("a.yaml"), dir.join("b.yaml"));
        fs::write(&a, SUB).unwrap();
        fs::write(&b, OTHER).unwrap();
        let first = core.import_file(&a, None).await.unwrap();
        let second = core.import_file(&b, None).await.unwrap();
        let (id_a, id_b) = (
            second.store.subscriptions[0].id.clone(),
            second.store.subscriptions[1].id.clone(),
        );
        assert_eq!(first.store.active.as_deref(), Some(id_a.as_str()));

        // The one running stays.
        let err = core.remove(&id_a).await.unwrap_err();
        assert!(matches!(err, CoreError::ActiveSubscription), "{err:?}");

        let switched = core.switch(&id_b).await.unwrap();
        assert!(switched.reload && switched.name == "b");
        assert_eq!(switched.store.active.as_deref(), Some(id_b.as_str()));
        assert!(config_of(&dir).starts_with(OTHER));
        // The same one again changes nothing.
        let again = core.switch(&id_b).await.unwrap();
        assert!(!again.reload);
        assert!(matches!(
            core.switch("nope").await,
            Err(CoreError::NoSubscription)
        ));

        // What a start of the kernel writes is the one in use.
        fs::write(dir.join("core/home/config.yaml"), "stale").unwrap();
        core.write_config().unwrap();
        assert!(config_of(&dir).starts_with(OTHER));

        // Now the other can go, with its files.
        let removed = core.remove(&id_a).await.unwrap();
        assert_eq!(removed.store.subscriptions.len(), 1);
        assert!(!dir.join(format!("core/profiles/{id_a}.yaml")).exists());
        assert_eq!(core.subscriptions().unwrap(), removed.store);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn without_a_kernel_nothing_can_be_checked_and_nothing_is_imported() {
        let dir = scratch("sub-nobinary");
        let recorder = kernel();
        let core = crate::core::testing::core_without_binary(&dir, free_port(), &recorder);
        let file = dir.join("a.yaml");
        fs::write(&file, SUB).unwrap();

        let err = core.import_file(&file, None).await.unwrap_err();

        assert!(matches!(err, CoreError::NoBinary { path: None }), "{err:?}");
        assert_eq!(core.subscriptions().unwrap(), Store::default());
        fs::remove_dir_all(dir).unwrap();
    }

    /// A key pair as `age-keygen` writes it: the secret line, the recipient.
    fn age_pair() -> (String, age::x25519::Identity) {
        use age::secrecy::ExposeSecret;
        let identity = age::x25519::Identity::generate();
        (identity.to_string().expose_secret().to_owned(), identity)
    }

    async fn serve_bytes(body: Vec<u8>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sub"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        server
    }

    /// Every file the core keeps, as a list of paths, to say that a failed
    /// import left none of it behind.
    fn everything_in(dir: &Path) -> Vec<String> {
        let mut found = vec![];
        let mut pending = vec![dir.to_owned()];
        while let Some(next) = pending.pop() {
            for entry in fs::read_dir(&next).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push(path.strip_prefix(dir).unwrap().display().to_string());
                }
            }
        }
        found.sort();
        found
    }

    #[tokio::test]
    async fn an_encrypted_subscription_needs_its_key_and_is_read_with_it_once_given() {
        let (core, _, dir) = core("sub-age");
        let (key, identity) = age_pair();
        let server =
            serve_bytes(age::encrypt(&identity.to_public(), SUB.as_bytes()).unwrap()).await;
        let before = everything_in(&dir);

        // No key, then somebody else's, then something that is no key at all.
        let err = core.import_url(&url(&server), None).await.unwrap_err();
        assert!(matches!(err, CoreError::AgeKeyNeeded), "{err:?}");
        let (other, _) = age_pair();
        let err = core
            .import_url(&url(&server), Some(&other))
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::Age(crate::core::Fault::WrongKey)),
            "{err:?}"
        );
        let err = core
            .import_url(&url(&server), Some("hunter2"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::Age(crate::core::Fault::NotAKey)),
            "{err:?}"
        );
        // None of those left a file, or an entry, or a config to run.
        assert_eq!(everything_in(&dir), before);

        let imported = core
            .import_url(&url(&server), Some(&format!("{key}\n")))
            .await
            .unwrap();

        let sub = &imported.store.subscriptions[0];
        assert_eq!(sub.age_key.as_deref(), Some(key.as_str()));
        // The kernel is given what was inside, not the ciphertext.
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/profiles/{}.yaml", sub.id))).unwrap(),
            SUB
        );
        assert!(config_of(&dir).contains("1.2.3.4"));
        let stored = dir.join("core/subscriptions.toml");
        assert!(fs::read_to_string(&stored).unwrap().contains(&key));
        assert_eq!(
            fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_update_of_an_encrypted_subscription_uses_the_key_kept_with_it() {
        let (core, _, dir) = core("sub-age-update");
        let (key, identity) = age_pair();
        let server =
            serve_bytes(age::encrypt(&identity.to_public(), SUB.as_bytes()).unwrap()).await;
        let imported = core.import_url(&url(&server), Some(&key)).await.unwrap();
        let id = imported.store.subscriptions[0].id.clone();

        // The next one is armored, as `age -a` writes it.
        server.reset().await;
        let armored = age::encrypt_and_armor(&identity.to_public(), OTHER.as_bytes()).unwrap();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(armored))
            .mount(&server)
            .await;
        core.update(&id).await.unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/profiles/{id}.yaml"))).unwrap(),
            OTHER
        );

        // Then the key is changed on the server's side: nothing is lost.
        let (_, newer) = age_pair();
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(age::encrypt(&newer.to_public(), SUB.as_bytes()).unwrap()),
            )
            .mount(&server)
            .await;
        let err = core.update(&id).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Age(crate::core::Fault::WrongKey)),
            "{err:?}"
        );
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/profiles/{id}.yaml"))).unwrap(),
            OTHER
        );

        // And when it stops being encrypted, the key is not kept for nothing.
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SUB))
            .mount(&server)
            .await;
        let updated = core.update(&id).await.unwrap();
        assert_eq!(updated.store.subscriptions[0].age_key, None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_encrypted_file_is_imported_and_what_is_inside_must_still_be_a_subscription() {
        let (core, _, dir) = core("sub-age-file");
        let (key, identity) = age_pair();
        let good = dir.join("good.yaml.age");
        fs::write(
            &good,
            age::encrypt(&identity.to_public(), SUB.as_bytes()).unwrap(),
        )
        .unwrap();
        let junk = dir.join("junk.age");
        fs::write(
            &junk,
            age::encrypt(&identity.to_public(), b"<html>hi</html>").unwrap(),
        )
        .unwrap();
        let before = everything_in(&dir);

        let err = core.import_file(&junk, Some(&key)).await.unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
        assert_eq!(everything_in(&dir), before);

        let imported = core.import_file(&good, Some(&key)).await.unwrap();
        assert_eq!(
            imported.store.subscriptions[0].age_key.as_deref(),
            Some(key.as_str())
        );
        assert!(config_of(&dir).contains("1.2.3.4"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_subscription_is_renamed_and_given_an_interval_without_touching_its_files() {
        let (core, _, dir) = core("sub-edit");
        let server = serve(SUB, &[]).await;
        let id = core
            .import_url(&url(&server), None)
            .await
            .unwrap()
            .store
            .subscriptions[0]
            .id
            .clone();
        let before = everything_in(&dir);
        let config = config_of(&dir);

        let renamed = core.rename(&id, "  家里的机场 ").await.unwrap();
        assert_eq!(renamed.name, "家里的机场");
        assert!(!renamed.reload);
        let timed = core.set_interval(&id, 6).await.unwrap();
        assert_eq!(timed.store.subscriptions[0].interval_hours, 6);

        // Kept in the list, and nowhere else: not a file is made or changed.
        let stored = core.subscriptions().unwrap();
        assert_eq!(stored.subscriptions[0].name, "家里的机场");
        assert_eq!(stored.subscriptions[0].interval_hours, 6);
        assert_eq!(everything_in(&dir), before);
        assert_eq!(config_of(&dir), config);
        assert!(matches!(
            core.rename("nope", "x").await,
            Err(CoreError::NoSubscription)
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn another_key_is_tried_on_the_subscription_and_kept_only_if_it_opens_it() {
        let (core, _, dir) = core("sub-rekey");
        let (key, identity) = age_pair();
        let server =
            serve_bytes(age::encrypt(&identity.to_public(), SUB.as_bytes()).unwrap()).await;
        let id = core
            .import_url(&url(&server), Some(&key))
            .await
            .unwrap()
            .store
            .subscriptions[0]
            .id
            .clone();

        // The server changed its key; the one kept with the subscription
        // no longer opens it, and neither does a wrong or a bad one.
        let (newer_key, newer) = age_pair();
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(age::encrypt(&newer.to_public(), OTHER.as_bytes()).unwrap()),
            )
            .mount(&server)
            .await;
        let (wrong, _) = age_pair();
        for bad in [wrong.as_str(), "hunter2"] {
            assert!(core.set_age_key(&id, bad).await.is_err(), "{bad}");
        }
        let stored = core.subscriptions().unwrap();
        assert_eq!(
            stored.subscriptions[0].age_key.as_deref(),
            Some(key.as_str())
        );
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/profiles/{id}.yaml"))).unwrap(),
            SUB
        );

        let done = core.set_age_key(&id, &newer_key).await.unwrap();
        assert_eq!(
            done.store.subscriptions[0].age_key.as_deref(),
            Some(newer_key.as_str())
        );
        assert_eq!(
            fs::read_to_string(dir.join(format!("core/profiles/{id}.yaml"))).unwrap(),
            OTHER
        );
        // It runs, so the kernel has to read it again.
        assert!(done.reload);
        fs::remove_dir_all(dir).unwrap();
    }
}

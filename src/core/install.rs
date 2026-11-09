//! Getting the kernel: the release to take, the file to download from it,
//! the replacement of the installed binary (plan §6), and the copy of the
//! kernel shipped with tabby.

use std::{
    fmt::Write as _,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{Action, CoreError, ManagedCore, Probe, Step, runner::Cmd};

/// Where the releases of the kernel are looked up.
pub const API_BASE: &str = "https://api.github.com";

pub(super) const LATEST_PATH: &str = "/repos/MetaCubeX/mihomo/releases/latest";

/// A release of the kernel, as GitHub lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag: String,
    pub assets: Vec<Asset>,
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    /// `sha256:<hex>` of the file.
    #[serde(default)]
    pub digest: Option<String>,
}

/// The newest release.
pub async fn latest(http: &reqwest::Client, api_base: &str) -> Result<Release, CoreError> {
    let failed = |err: reqwest::Error| CoreError::Release(describe(&err));
    let response = http
        .get(format!("{}{LATEST_PATH}", api_base.trim_end_matches('/')))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        // GitHub says why in `message`: a rate limit most often.
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|json| json["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| status.to_string());
        return Err(CoreError::Release(message));
    }
    response.json().await.map_err(failed)
}

/// `err` and its causes, which reqwest keeps out of its own message
/// ("Connection refused").
pub(super) fn describe(err: &reqwest::Error) -> String {
    let mut message = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        let _ = write!(message, ": {cause}");
        source = cause.source();
    }
    message
}

/// Where a release of tabby that ships a kernel puts it, given where tabby
/// itself is: `<prefix>/lib/tabby/mihomo` for `<prefix>/bin/tabby`.
pub fn shipped_next_to(exe: &Path) -> Option<PathBuf> {
    Some(exe.parent()?.parent()?.join("lib/tabby/mihomo"))
}

/// What tells one copy of the shipped kernel from another without reading
/// it: its size and when it was written. `None` when it is not a file.
fn fingerprint(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
    let written = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("{} {}", meta.len(), written.as_nanos()))
}

/// Copies `shipped` over `kept` in one step, through `kept.new`.
fn copy_in(shipped: &Path, kept: &Path) -> io::Result<()> {
    let dir = kept.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let new = dir.join("mihomo.new");
    let result = fs::copy(shipped, &new)
        .and_then(|_| {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&new, fs::Permissions::from_mode(0o755))
        })
        .and_then(|()| fs::rename(&new, kept));
    if result.is_err() {
        let _ = fs::remove_file(&new);
    }
    result
}

/// The x86-64 microarchitecture level the CPU in `cpuinfo` (the text of
/// `/proc/cpuinfo`) supports: 3, 2, or 1.
fn amd64_level(cpuinfo: &str) -> u8 {
    const V3: [&str; 8] = [
        "avx2", "bmi1", "bmi2", "fma", "movbe", "abm", "f16c", "xsave",
    ];
    const V2: [&str; 4] = ["sse4_2", "popcnt", "ssse3", "cx16"];
    let flags: Vec<&str> = cpuinfo
        .lines()
        .find(|line| line.starts_with("flags"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, flags)| flags.split_whitespace().collect())
        .unwrap_or_default();
    let has = |needed: &[&str]| needed.iter().all(|flag| flags.contains(flag));
    if has(&V2) && has(&V3) {
        3
    } else if has(&V2) {
        2
    } else {
        1
    }
}

/// What the machine is: its architecture as Rust names it, and its CPU as
/// `/proc/cpuinfo` describes it.
#[derive(Debug, Clone)]
pub struct Target {
    pub arch: String,
    pub cpuinfo: String,
}

impl Target {
    pub fn current() -> Self {
        Self {
            arch: std::env::consts::ARCH.to_owned(),
            cpuinfo: fs::read_to_string("/proc/cpuinfo").unwrap_or_default(),
        }
    }

    /// The file names that hold the kernel for this machine in release
    /// `tag`, best first. The `amd64-compatible` build is left out: the
    /// build script says its name goes away.
    fn asset_names(&self, tag: &str) -> Vec<String> {
        match self.arch.as_str() {
            "x86_64" => (1..=amd64_level(&self.cpuinfo))
                .rev()
                .map(|level| format!("mihomo-linux-amd64-v{level}-{tag}.gz"))
                .collect(),
            "aarch64" => vec![format!("mihomo-linux-arm64-{tag}.gz")],
            _ => vec![],
        }
    }
}

/// The file of `release` to download for `target`.
pub fn choose<'a>(release: &'a Release, target: &Target) -> Result<&'a Asset, CoreError> {
    target
        .asset_names(&release.tag)
        .iter()
        .find_map(|name| release.assets.iter().find(|asset| asset.name == *name))
        .ok_or_else(|| CoreError::NoAsset {
            arch: target.arch.clone(),
        })
}

/// Downloads `asset` into `to`, checking it against its `digest` as it
/// arrives. `progress` hears the share done, when the size is known. A file
/// that does not match is removed.
pub async fn download(
    http: &reqwest::Client,
    asset: &Asset,
    to: &Path,
    progress: impl Fn(Option<u8>),
) -> Result<(), CoreError> {
    let expected = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or_else(|| CoreError::Checksum(format!("{} has no digest", asset.name)))?;
    let failed = |err: reqwest::Error| CoreError::Download(describe(&err));
    let io_failed = |err: io::Error| CoreError::Download(err.to_string());

    let mut response = http.get(&asset.url).send().await.map_err(failed)?;
    if !response.status().is_success() {
        return Err(CoreError::Download(response.status().to_string()));
    }
    let total = response.content_length().filter(|total| *total > 0);
    let mut file = File::create(to).map_err(io_failed)?;
    let mut hasher = Sha256::new();
    let (mut done, mut shown) = (0u64, None);
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(io_failed)?;
        done += chunk.len() as u64;
        if let Some(total) = total {
            let percent = u8::try_from((done * 100 / total).min(100)).unwrap_or(100);
            if shown != Some(percent) {
                shown = Some(percent);
                progress(shown);
            }
        }
    }
    file.sync_all().map_err(io_failed)?;
    drop(file);
    let got = hasher
        .finalize()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    if !got.eq_ignore_ascii_case(expected) {
        let _ = fs::remove_file(to);
        return Err(CoreError::Checksum(format!(
            "{}: expected sha256 {expected}, got {got}",
            asset.name
        )));
    }
    Ok(())
}

/// Unpacks the gzip file `gz` into the executable `out`.
pub fn unpack(gz: &Path, out: &Path) -> io::Result<()> {
    let mut decoder = GzDecoder::new(File::open(gz)?);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(out)?;
    io::copy(&mut decoder, &mut file)?;
    file.sync_all()
}

/// The version a kernel prints for `-v`: `Mihomo Meta v1.19.33 linux amd64
/// with go1.26 …` has `v1.19.33`.
pub fn version_of(output: &str) -> Option<&str> {
    output.split_whitespace().find(|word| {
        word.strip_prefix('v')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// What a look at the newest release says about the installed kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upgrade {
    /// The installed kernel is the newest: its version.
    Current(String),
    /// A newer release is out.
    Available(Release),
}

impl ManagedCore {
    /// Puts the kernel shipped with tabby where tabby keeps its own: tabby
    /// can update that one and give it capabilities, which it cannot do to a
    /// file of the installed package (and an update of the package would
    /// take the capabilities away again).
    ///
    /// Done when there is no kernel there yet, and again when the shipped
    /// one is another than the one copied (tabby was updated). A kernel
    /// tabby downloaded is left alone, and so is `[core].binary`.
    pub(super) async fn seed(&self) -> Result<(), CoreError> {
        if self.binary.is_some() {
            return Ok(());
        }
        let Some(shipped) = self.shipped.clone() else {
            return Ok(());
        };
        let Some(mark) = fingerprint(&shipped) else {
            return Ok(());
        };
        let (kept, record) = (self.paths.bin(), self.paths.seeded());
        if kept.is_file() {
            match fs::read_to_string(&record) {
                Ok(copied) if copied == mark => return Ok(()),
                Ok(_) => {}
                // Not a copy of the shipped kernel: downloaded.
                Err(_) => return Ok(()),
            }
        }
        let into = kept.clone();
        tokio::task::spawn_blocking(move || copy_in(&shipped, &into))
            .await
            .map_err(io::Error::other)
            .and_then(std::convert::identity)
            .map_err(|err| CoreError::io("cannot copy the kernel shipped with tabby", err))?;
        fs::write(&record, mark)
            .map_err(|err| CoreError::io("cannot note where the kernel came from", err))
    }

    /// The ways to reach GitHub, in the order to try them: through the
    /// managed kernel's mixed port when it runs (GitHub cannot be reached
    /// without a proxy in some places), then directly. The controller's own
    /// client ignores proxies on purpose; this one is for the internet.
    async fn routes(&self) -> Vec<reqwest::Client> {
        let mut routes = Vec::new();
        if self.probe().await == Probe::Ready
            && let Ok(proxy) =
                reqwest::Proxy::all(format!("http://127.0.0.1:{}", self.settings().mixed_port))
            && let Ok(client) = http(Some(proxy))
        {
            routes.push(client);
        }
        if let Ok(client) = http(None) {
            routes.push(client);
        }
        routes
    }

    /// The newest release of the kernel.
    pub async fn latest_release(&self) -> Result<Release, CoreError> {
        let mut failure = CoreError::Release("no way to reach the server".to_owned());
        for client in self.routes().await {
            match latest(&client, &self.release_api).await {
                Ok(release) => return Ok(release),
                Err(err) => failure = err,
            }
        }
        Err(failure)
    }

    /// The version of the installed kernel, as it says itself.
    pub async fn installed_version(&self) -> Option<String> {
        let binary = self.binary().ok()?;
        let output = self
            .exec(Cmd::new(&binary.to_string_lossy(), ["-v"]))
            .await
            .ok()?;
        version_of(&String::from_utf8_lossy(&output.stdout)).map(str::to_owned)
    }

    /// Whether a newer kernel than the installed one is out.
    pub async fn check_upgrade(&self) -> Result<Upgrade, CoreError> {
        let release = self.latest_release().await?;
        Ok(match self.installed_version().await {
            Some(version) if version == release.tag => Upgrade::Current(version),
            _ => Upgrade::Available(release),
        })
    }

    /// Downloads the kernel of `release` and puts it in place of the
    /// installed one (plan §6): into a file of its own first, checked
    /// against the release's digest as it arrives, unpacked, and made to
    /// say its version; only then does it replace the old one, in one step.
    /// A failure at any point leaves the old kernel as it was.
    pub async fn install(
        &self,
        release: &Release,
        progress: impl Fn(Step),
    ) -> Result<(), CoreError> {
        let asset = choose(release, &self.target)?;
        let bin = self.paths.bin();
        let dir = bin.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir)
            .map_err(|err| CoreError::io("cannot create the bin directory", err))?;
        let part = dir.join("mihomo.gz.part");
        let new = dir.join("mihomo.new");
        let result = self
            .fetch_and_replace(release, asset, &part, &new, &progress)
            .await;
        // Whatever is left of a failed attempt is of no use.
        let _ = fs::remove_file(&part);
        let _ = fs::remove_file(&new);
        result
    }

    async fn fetch_and_replace(
        &self,
        release: &Release,
        asset: &Asset,
        part: &Path,
        new: &Path,
        progress: &impl Fn(Step),
    ) -> Result<(), CoreError> {
        progress(Step::Downloading(None));
        let mut failure = CoreError::Download("no way to reach the server".to_owned());
        let mut downloaded = false;
        for client in self.routes().await {
            match download(&client, asset, part, |percent| {
                progress(Step::Downloading(percent));
            })
            .await
            {
                Ok(()) => {
                    downloaded = true;
                    break;
                }
                // A file that came and is wrong is not for another route to
                // put right.
                Err(err @ CoreError::Checksum(_)) => return Err(err),
                Err(err) => failure = err,
            }
        }
        if !downloaded {
            return Err(failure);
        }
        progress(Step::Installing);
        let (gz, out) = (part.to_owned(), new.to_owned());
        tokio::task::spawn_blocking(move || unpack(&gz, &out))
            .await
            .map_err(io::Error::other)
            .and_then(std::convert::identity)
            .map_err(|err| CoreError::io("cannot unpack the kernel", err))?;
        self.verify(new, &release.tag).await?;
        fs::rename(new, self.paths.bin())
            .map_err(|err| CoreError::io("cannot put the kernel in place", err))?;
        // A downloaded kernel is not a copy of the shipped one: an update of
        // tabby does not put the shipped one back over it.
        let _ = fs::remove_file(self.paths.seeded());
        Ok(())
    }

    /// Runs `binary -v` and requires it to be the release `tag`: a file that
    /// does not run, or is another version, is not installed.
    async fn verify(&self, binary: &Path, tag: &str) -> Result<(), CoreError> {
        let failed = |message: String| CoreError::Command {
            action: Action::Install,
            message,
        };
        let output = self
            .exec(Cmd::new(&binary.to_string_lossy(), ["-v"]))
            .await
            .map_err(|err| failed(err.to_string()))?;
        if !output.status.success() {
            return Err(failed(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let said = String::from_utf8_lossy(&output.stdout);
        match version_of(&said) {
            Some(version) if version == tag => Ok(()),
            other => Err(failed(format!(
                "the downloaded kernel is {}, not {tag}",
                other.unwrap_or("of no known version")
            ))),
        }
    }
}

/// The client for GitHub, through `proxy` when given. It does not read the
/// proxy variables: the proxy is the managed kernel or none.
fn http(proxy: Option<reqwest::Proxy>) -> reqwest::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .user_agent(concat!("tabby/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600));
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(flags: &str) -> String {
        format!("processor : 0\nmodel name : x\nflags\t\t: fpu {flags}\n")
    }

    const V2: &str = "sse4_2 popcnt ssse3 cx16";
    const V3: &str = "avx2 bmi1 bmi2 fma movbe abm f16c xsave";

    #[test]
    fn the_cpu_level_needs_every_flag_of_its_level() {
        assert_eq!(amd64_level(&cpu(&format!("{V2} {V3}"))), 3);
        assert_eq!(amd64_level(&cpu(V2)), 2);
        // One flag short of v3 is v2; one short of v2 is v1.
        let almost_v3 = V3.replace("movbe", "");
        assert_eq!(amd64_level(&cpu(&format!("{V2} {almost_v3}"))), 2);
        let almost_v2 = V2.replace("cx16", "");
        assert_eq!(amd64_level(&cpu(&format!("{almost_v2} {V3}"))), 1);
        assert_eq!(amd64_level(""), 1);
        // A flag in another line is not a flag of the CPU.
        assert_eq!(amd64_level(&format!("model name : {V2}\n")), 1);
    }

    fn release(names: &[&str]) -> Release {
        Release {
            tag: "v1.2.3".to_owned(),
            assets: names
                .iter()
                .map(|name| Asset {
                    name: (*name).to_owned(),
                    url: format!("https://example.test/{name}"),
                    digest: None,
                })
                .collect(),
        }
    }

    const ALL: [&str; 7] = [
        "mihomo-linux-amd64-v3-v1.2.3.gz",
        "mihomo-linux-amd64-v2-v1.2.3.gz",
        "mihomo-linux-amd64-v1-v1.2.3.gz",
        "mihomo-linux-amd64-compatible-v1.2.3.gz",
        "mihomo-linux-amd64-v3-go123-v1.2.3.gz",
        "mihomo-linux-arm64-v1.2.3.gz",
        "mihomo-windows-amd64-v1.2.3.zip",
    ];

    fn chosen(arch: &str, flags: &str, names: &[&str]) -> Result<String, CoreError> {
        let target = Target {
            arch: arch.to_owned(),
            cpuinfo: cpu(flags),
        };
        choose(&release(names), &target).map(|asset| asset.name.clone())
    }

    #[test]
    fn the_best_build_the_cpu_runs_is_chosen_and_never_a_look_alike() {
        let all = format!("{V2} {V3}");
        assert_eq!(
            chosen("x86_64", &all, &ALL).unwrap(),
            "mihomo-linux-amd64-v3-v1.2.3.gz"
        );
        assert_eq!(
            chosen("x86_64", V2, &ALL).unwrap(),
            "mihomo-linux-amd64-v2-v1.2.3.gz"
        );
        assert_eq!(
            chosen("x86_64", "", &ALL).unwrap(),
            "mihomo-linux-amd64-v1-v1.2.3.gz"
        );
        // A release without the v3 build gives a v3 CPU the v2 one.
        assert_eq!(
            chosen("x86_64", &all, &ALL[1..]).unwrap(),
            "mihomo-linux-amd64-v2-v1.2.3.gz"
        );
        assert_eq!(
            chosen("aarch64", "", &ALL).unwrap(),
            "mihomo-linux-arm64-v1.2.3.gz"
        );
    }

    #[test]
    fn a_machine_without_a_build_says_which_architecture() {
        for (arch, names) in [("riscv64", &ALL[..]), ("x86_64", &ALL[5..])] {
            let err = chosen(arch, "", names).unwrap_err();
            assert!(
                matches!(&err, CoreError::NoAsset { arch: reported } if reported == arch),
                "{err:?}"
            );
        }
    }

    #[test]
    fn the_version_is_the_word_that_looks_like_one() {
        assert_eq!(
            version_of("Mihomo Meta v1.19.33 linux amd64 with go1.26.10 Wed Oct 21\n"),
            Some("v1.19.33")
        );
        assert_eq!(version_of("Mihomo Meta alpha-g1234 linux"), None);
        assert_eq!(version_of("vendor v2.0.0"), Some("v2.0.0"));
    }

    use crate::core::testing::{
        self, KERNEL, Recorder, TAG, answer, free_port, gzip, release_server as server, scratch,
        sha256,
    };
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// A managed core with no kernel yet, looking releases up at `server`;
    /// every `-v` says `version_said`.
    fn core(
        dir: &Path,
        server: &MockServer,
        exit: i32,
        version_said: &'static str,
    ) -> (ManagedCore, Arc<Recorder>) {
        let recorder = Recorder::new(move |_| answer(exit, version_said));
        let mut core = testing::core_without_binary(dir, free_port(), &recorder);
        core.release_api = server.uri();
        core.target = Target {
            arch: "x86_64".to_owned(),
            cpuinfo: String::new(),
        };
        (core, recorder)
    }

    /// Puts a kernel of content `old` where tabby keeps its own.
    fn installed(dir: &Path, old: &str) -> std::path::PathBuf {
        let bin = dir.join("core/bin/mihomo");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, old).unwrap();
        bin
    }

    const SAYS_TAG: &str = "Mihomo Meta v1.2.3 linux amd64 with go1.26\n";

    #[tokio::test]
    async fn a_release_is_downloaded_checked_unpacked_and_put_in_place() {
        let dir = scratch("install");
        let packed = gzip(KERNEL);
        let server = server(Some(format!("sha256:{}", sha256(&packed))), packed).await;
        let (core, recorder) = core(&dir, &server, 0, SAYS_TAG);
        let steps = Mutex::new(vec![]);

        let release = core.latest_release().await.unwrap();
        assert_eq!(release.tag, TAG);
        core.install(&release, |step| steps.lock().unwrap().push(step))
            .await
            .unwrap();

        let bin = dir.join("core/bin/mihomo");
        assert_eq!(fs::read(&bin).unwrap(), KERNEL);
        assert_eq!(
            fs::metadata(&bin).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // Nothing of the attempt is left next to it.
        assert_eq!(fs::read_dir(bin.parent().unwrap()).unwrap().count(), 1);
        // It was made to say its version before it replaced anything.
        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].program.as_str(), calls[0].args.as_slice()),
            (
                dir.join("core/bin/mihomo.new").to_str().unwrap(),
                ["-v".to_owned()].as_slice()
            )
        );
        let steps = steps.into_inner().unwrap();
        assert_eq!(steps.first(), Some(&Step::Downloading(None)));
        // The download reports its progress up to the end, and unpacking and
        // the check come last.
        assert!(steps.contains(&Step::Downloading(Some(100))), "{steps:?}");
        assert_eq!(steps.last(), Some(&Step::Installing));
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_download_that_does_not_match_its_digest_is_refused_and_the_old_kernel_stays() {
        let dir = scratch("install-tampered");
        let bin = installed(&dir, "old");
        // The digest is of another file than the one served.
        let server = server(
            Some(format!("sha256:{}", sha256(&gzip(b"the real one")))),
            gzip(KERNEL),
        )
        .await;
        let (core, recorder) = core(&dir, &server, 0, SAYS_TAG);
        let release = core.latest_release().await.unwrap();

        let err = core.install(&release, |_| {}).await.unwrap_err();

        assert!(matches!(err, CoreError::Checksum(_)), "{err:?}");
        assert_eq!(fs::read_to_string(&bin).unwrap(), "old");
        assert_eq!(fs::read_dir(bin.parent().unwrap()).unwrap().count(), 1);
        // The file was never run.
        assert_eq!(recorder.calls(), Vec::<Cmd>::new());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_release_that_gives_no_digest_is_not_trusted() {
        let dir = scratch("install-no-digest");
        let bin = installed(&dir, "old");
        let server = server(None, gzip(KERNEL)).await;
        let (core, _) = core(&dir, &server, 0, SAYS_TAG);
        let release = core.latest_release().await.unwrap();

        let err = core.install(&release, |_| {}).await.unwrap_err();

        assert!(matches!(err, CoreError::Checksum(_)), "{err:?}");
        assert_eq!(fs::read_to_string(&bin).unwrap(), "old");
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_kernel_that_does_not_run_or_is_another_version_is_not_put_in_place() {
        for (exit, said) in [
            (0, "Mihomo Meta v9.9.9 linux amd64\n"),
            (0, "something else\n"),
            (1, ""),
        ] {
            let dir = scratch("install-wrong");
            let bin = installed(&dir, "old");
            let packed = gzip(KERNEL);
            let server = server(Some(format!("sha256:{}", sha256(&packed))), packed).await;
            let (core, _) = core(&dir, &server, exit, said);
            let release = core.latest_release().await.unwrap();

            let err = core.install(&release, |_| {}).await.unwrap_err();

            assert!(
                matches!(
                    err,
                    CoreError::Command {
                        action: Action::Install,
                        ..
                    }
                ),
                "{said:?}: {err:?}"
            );
            assert_eq!(fs::read_to_string(&bin).unwrap(), "old");
            assert_eq!(fs::read_dir(bin.parent().unwrap()).unwrap().count(), 1);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    async fn a_refused_lookup_says_what_github_says() {
        let dir = scratch("install-limit");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(LATEST_PATH))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"message": "API rate limit exceeded"})),
            )
            .mount(&server)
            .await;
        let (core, _) = core(&dir, &server, 0, SAYS_TAG);

        let err = core.latest_release().await.unwrap_err();

        assert!(
            matches!(&err, CoreError::Release(message) if message == "API rate limit exceeded"),
            "{err:?}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_installed_kernel_is_current_only_when_it_says_the_release_tag() {
        let dir = scratch("install-current");
        installed(&dir, "kernel");
        let server = server(None, vec![]).await;
        let (core, _) = core(&dir, &server, 0, SAYS_TAG);
        assert_eq!(
            core.check_upgrade().await.unwrap(),
            Upgrade::Current(TAG.to_owned())
        );

        let (older, _) = core_older(&dir, &server);
        assert!(matches!(
            older.check_upgrade().await.unwrap(),
            Upgrade::Available(release) if release.tag == TAG
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    /// Like [`core`], over the same directory, with a kernel that says it is
    /// older.
    fn core_older(dir: &Path, server: &MockServer) -> (ManagedCore, Arc<Recorder>) {
        core(dir, server, 0, "Mihomo Meta v1.2.2 linux amd64\n")
    }

    /// Ships a kernel of content `content` with the tabby whose core is in
    /// `dir`.
    fn ship(dir: &Path, content: &str) -> PathBuf {
        let shipped = shipped_next_to(&dir.join("prefix/bin/tabby")).unwrap();
        fs::create_dir_all(shipped.parent().unwrap()).unwrap();
        fs::write(&shipped, content).unwrap();
        shipped
    }

    #[tokio::test]
    async fn the_shipped_kernel_is_copied_in_once_and_again_only_when_tabby_brings_another() {
        let dir = scratch("seed");
        let server = server(None, vec![]).await;
        let (mut core, _) = core(&dir, &server, 0, SAYS_TAG);
        core.shipped = Some(ship(&dir, "v1"));
        let kept = dir.join("core/bin/mihomo");

        core.seed().await.unwrap();

        assert_eq!(fs::read_to_string(&kept).unwrap(), "v1");
        assert_eq!(
            fs::metadata(&kept).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(core.binary().unwrap(), kept);
        assert!(!core.is_user_binary());

        // The same shipped kernel again: the copy, which may have been given
        // capabilities since, stays as it is.
        fs::write(&kept, "authorized").unwrap();
        core.seed().await.unwrap();
        assert_eq!(fs::read_to_string(&kept).unwrap(), "authorized");

        // A newer tabby ships another one.
        ship(&dir, "v2, a newer one");
        core.seed().await.unwrap();
        assert_eq!(fs::read_to_string(&kept).unwrap(), "v2, a newer one");
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_downloaded_kernel_is_never_replaced_by_the_shipped_one() {
        let dir = scratch("seed-downloaded");
        let packed = gzip(KERNEL);
        let server = server(Some(format!("sha256:{}", sha256(&packed))), packed).await;
        let (mut core, _) = core(&dir, &server, 0, SAYS_TAG);

        // Downloaded before tabby shipped any.
        let kept = installed(&dir, "downloaded");
        core.shipped = Some(ship(&dir, "v1"));
        core.seed().await.unwrap();
        assert_eq!(fs::read_to_string(&kept).unwrap(), "downloaded");

        // Downloaded over a copy of the shipped one.
        fs::remove_file(&kept).unwrap();
        core.seed().await.unwrap();
        assert_eq!(fs::read_to_string(&kept).unwrap(), "v1");
        let release = core.latest_release().await.unwrap();
        core.install(&release, |_| {}).await.unwrap();
        ship(&dir, "v2, a newer one");
        core.seed().await.unwrap();

        assert_eq!(fs::read(&kept).unwrap(), KERNEL);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_configured_kernel_leaves_the_shipped_one_unused() {
        let dir = scratch("seed-configured");
        let server = server(None, vec![]).await;
        let (mut core, _) = core(&dir, &server, 0, SAYS_TAG);
        core.shipped = Some(ship(&dir, "v1"));
        let own = dir.join("own-mihomo");
        fs::write(&own, "mine").unwrap();
        core.binary = Some(own.clone());

        core.seed().await.unwrap();

        assert!(!dir.join("core/bin/mihomo").exists());
        assert_eq!(core.binary().unwrap(), own);
        fs::remove_dir_all(dir).unwrap();
    }
}

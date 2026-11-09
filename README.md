# tabby

终端里的 [mihomo](https://github.com/MetaCubeX/mihomo)（Clash.Meta）控制台。

基于 [ratatui](https://ratatui.rs)，通过 External Controller 的 REST 与 WebSocket 接口工作：实时流量、代理切换与测速、Providers、规则、连接、日志和内核设置，无需浏览器。界面为中文，键盘与鼠标均可操作。

```text
◆ tabby  mihomo v1.19.33   RULE             ↑      0 B/s   ↓      0 B/s   ● test
1  2  3  4  5  6  7
────────────────────────────────────────────────────────────────────────────────
▾ 节点选择  SEL  → 自动选择
╭ 节点选择  Selector · 13 个节点 · 默认顺序 ─────────────────────── 1–12 / 13 ▾╮
│╭────────────────────────╮╭────────────────────────╮╭────────────────────────╮│
││自动选择               ✓││故障转移                ││负载均衡                ││
││URLTest · UDP          —││Fallback · UDP         —││LoadBalance · UDP      —││
│╰────────────────────────╯╰────────────────────────╯╰────────────────────────╯│
│╭────────────────────────╮╭────────────────────────╮╭────────────────────────╮│
││直连 A                  ││直连 B                  ││香港 01                 ││
││Direct · UDP      250 ms││Direct · UDP      252 ms││Direct · UDP           —││
│╰━━━━━━━━━━━━────────────╯╰━━━━━━━━━━━━────────────╯╰────────────────────────╯│
│╭────────────────────────╮╭────────────────────────╮╭────────────────────────╮│
││香港 02                 ││死节点 1                ││死节点 2                ││
││Direct · UDP           —││Socks5              失败││Http                   —││
│╰────────────────────────╯╰────────────────────────╯╰────────────────────────╯│
╰──────────────────────────────────────────────────────────────────────────────╯
────────────────────────────────────────────────────────────────────────────────
Enter 选组   t 测速   T 整组测速   s 排序   / 过滤   …   ? 帮助   q 退出
```

<sub>80×24 终端中的代理页，连接本地 mihomo 测试实例。宽度 ≥ 100 列时左侧显示代理组列表。</sub>

## 特性

- **八个页面**：概览、代理、Providers、规则、连接、日志、设置、订阅。
- **托管内核**（可选）：tabby 自己下载并运行 mihomo，管理订阅，设置 TUN、登录时启动与系统代理。
- **实时数据**：流量、内存、连接与日志走 WebSocket；断线自动退避重连。
- **多配置**：在多个控制器之间一键切换。
- **主题**：内置 7 套，支持自定义主题文件与逐色覆盖，主题弹窗实时预览。
- **响应式**：自适应 50×15 以上的任意终端尺寸。
- **纯 Rust**：单一二进制，TLS 使用 rustls，不依赖 OpenSSL。

## 安装

**发布包**（Linux x86_64）：`tabby-<版本>-<平台>.tar.gz` 只有 tabby；`tabby-<版本>-<平台>-with-mihomo.tar.gz` 另带一个固定版本的 mihomo（`lib/tabby/mihomo`），托管内核不需要再下载。两种包的目录结构都和安装前缀一致，解压到 `~/.local` 或 `/usr/local`：

```sh
tar -xzf tabby-0.1.0-x86_64-unknown-linux-gnu-with-mihomo.tar.gz -C ~/.local --strip-components=1
# 得到 ~/.local/bin/tabby、~/.local/lib/tabby/mihomo、~/.local/share/licenses/tabby/…
```

**从源码**：需要 Rust 1.95 或更新版本（edition 2024）。开发与测试在 Linux 上进行，其他平台未验证。

```sh
cargo install --path .      # 安装到 ~/.cargo/bin/tabby
cargo build --release       # 或只编译：target/release/tabby
```

## 使用

```sh
tabby                                              # http://127.0.0.1:9090，无 secret
tabby --url http://192.168.1.1:9090 --secret xxx
tabby --profile router                             # 使用 config.toml 中的 [profiles.router]
TABBY_SECRET=xxx tabby --theme tokyo-night
```

| 选项        | 环境变量        | 说明                                       | 默认值                            |
| ----------- | --------------- | ------------------------------------------ | --------------------------------- |
| `--profile` | `TABBY_PROFILE` | `config.toml` 中的 profile 名              | 配置里的 `default`                |
| `--url`     | `TABBY_URL`     | 控制器地址，`http` / `https`，可带路径前缀 | `http://127.0.0.1:9090`           |
| `--secret`  | `TABBY_SECRET`  | 控制器 secret                              | 无                                |
| `--theme`   | `TABBY_THEME`   | 主题名或主题文件路径                       | 配置里的 `theme`，否则 `terminal` |

优先级：命令行 > 环境变量 > `config.toml` > 内置默认值；空字符串等同于未设置。命令行参数会出现在进程列表中，secret 建议放在环境变量或配置文件里。tabby 访问控制器时忽略 `HTTP_PROXY` 等系统代理变量，避免请求绕回被控制的内核。

## 配置

`config.toml` 位于 `$XDG_CONFIG_HOME/tabby/`（默认 `~/.config/tabby/`）。文件不存在时使用默认值；内容无法识别（未知的 `icons`、缺少 `url` 的 profile、非法颜色等）时，tabby 在进入全屏前报错退出，并指出出错的键。

```toml
default = "local"            # 启动使用的 profile
theme = "terminal"           # 内置主题名、themes/ 中的主题名，或主题文件路径
icons = "unicode"            # unicode | nerd
mouse = true                 # false 则不接管鼠标，保留终端原生选择

[delay]                      # 测速参数（以下为默认值）
url = "https://www.gstatic.com/generate_204"
timeout_ms = 5000

[profiles.local]
url = "http://127.0.0.1:9090"
secret = "your-secret"       # 明文保存，注意文件权限

[profiles.router]
url = "http://192.168.1.1:9090"

[theme_overrides]            # 覆盖当前主题中的单个颜色
accent = "#7aa2f7"
success = "light-green"
bg = "reset"
```

### 切换配置

`P` 打开 profile 弹窗。切换时以目标 profile 的 `url` 与 `secret` 重新连接，并丢弃旧连接上未完成的请求；当前页面、排序、日志级别、主题保持不变，其余状态重置。`--url` / `--secret` 只作用于启动时的连接，弹窗首行“启动参数指定”可切回。

## 托管内核

除了连接已有的控制器，tabby 也可以自己运行 mihomo：下载内核、管理订阅、设置 TUN、登录时启动、设置桌面系统代理。默认不启用，在 `config.toml` 里加一段 `[core]` 才会在 `P` 弹窗里出现 `托管内核` 一行；`default = "core"` 或 `--profile core` 启动时直接用它。

```toml
default = "core"             # 可选：启动时直接用托管内核

[core]                       # 有这一段才显示“托管内核”
binary = "/usr/bin/mihomo"   # 可选；不写则按下面的顺序找，都没有就由 tabby 下载
```

**内核从哪里来**，依次：

1. `[core].binary`：直接用它，不下载也不升级（设置页 `升级内核` 显示“由你自己管理”）。
2. 数据目录里 tabby 自己的 `bin/mihomo`：下载来的，或从随包内核复制来的。tabby 可以升级它、给它授权 TUN。
3. 随包内核 `<前缀>/lib/tabby/mihomo`（`<前缀>/bin/tabby` 旁边，发布包 `with-mihomo` 里的那个）：启动内核时复制到数据目录再运行，而不是直接运行安装目录里的文件（那里通常属于 root，tabby 改不了，包更新也会抹掉 `setcap`）。tabby 更新后随包内核变了，下次启动或重启内核时再复制一次；用 `升级内核` 下载过的不会被它覆盖。
4. `PATH` 里的 `mihomo`（例如发行版的包）：同 `binary`，不升级（“由你自己管理”）。只找绝对路径的目录。
5. 都没有：询问后从 GitHub 下载最新的 mihomo（校验 sha256，按 CPU 选 amd64 的 v1/v2/v3 构建）。托管内核运行时之后的升级经它自己的 mixed 端口下载。

没有 `[core]` 段时，托管内核的功能都不会出现（订阅页只显示一句说明），其余行为不变。

**数据目录**：`$XDG_DATA_HOME/tabby/core/`（默认 `~/.local/share/tabby/core/`）。内核和它的 `home/`（`config.yaml`、geo 数据、provider 文件）、订阅原文 `profiles/`、订阅列表 `subscriptions.toml`、托管内核的设置 `settings.toml`、`state.toml`（控制器端口与 secret）都在这里；含 secret、订阅 URL 或 age 密钥的文件权限是 0600。这些都由 tabby 维护，不要手动改 `config.yaml`：它是每次从当前订阅和 `settings.toml` 重新生成的。

**进程**：有 systemd 用户实例时，内核作为临时单元 `tabby-mihomo-<后缀>` 运行（`systemd-run --user`），崩溃自动拉起，日志在 `journalctl --user -u tabby-mihomo-…`；没有时作为独立进程运行，日志在 `core.log`，崩溃不会自动拉起（横幅提示 `内核已退出`，`r` 重启）。`q` 只退出 tabby，内核继续运行；设置页 `停止内核` 才停。下次启动 tabby 会接上正在运行的内核。

**订阅**（第 8 页）：`a` 输入订阅地址或文件路径导入；`Enter` 切换（重新生成 `config.yaml` 并让内核重载）；`u` / `U` 更新；`t` 开关自动更新（URL 订阅默认每 24 小时，`i` 改间隔，1–8760 小时；tabby 运行期间每分钟检查，失败 30 分钟内不重试）；`e` 改名；`K` 更换加密订阅的 age 私钥（用新私钥重新读一次订阅，打得开才保存）；`d` 删除。订阅内容用 `mihomo -t` 检查；分享链接列表会被包装成一个 provider 和一个 `节点选择` 组；age 加密的订阅（`age-encryption.org/v1`，二进制或 ASCII 封装，X25519 私钥）会提示输入私钥，私钥以 `*` 显示，并明文保存在 `subscriptions.toml` 里供后续更新使用（订阅页 `K` 可更换）。订阅原文不会被重新序列化：tabby 只删除自己接管的顶层键（端口、控制器、`secret`、`tun` 等）并追加一段覆盖块。

**设置页的“托管内核”组**：内核路径与版本、`升级内核`、`登录时启动`、`系统代理`、`DNS 覆盖`、`停止内核`。托管内核只开 Mixed 一个端口（默认 7890，被占用时在设置页改）；设置页和 `m` 的改动在生效后写进 `settings.toml`，重启后保留。

**DNS 覆盖**：设置页 `DNS 覆盖` 打开一个弹窗，列出可以覆盖订阅 `dns` 段的 11 个键（`listen`、`ipv6`、`prefer-h3`、`enhanced-mode`、`fake-ip-filter`、`fake-ip-filter-mode`、`default-nameserver`、`nameserver`、`fallback`、`proxy-server-nameserver`、`direct-nameserver`）。`Enter` 在开关和固定取值（`enhanced-mode`、`fake-ip-filter-mode`）上依次切换“跟随订阅 → 各个值 → 跟随订阅”，在文本和列表上打开输入框（列表用逗号或空格分隔，留空即恢复跟随订阅）。每次修改先用 `mihomo -t` 检查用新值生成的配置，内核不接受就保持原样并弹出它的报错；接受后写进 `settings.toml` 的 `[dns]`，关闭弹窗时让内核重载配置。`mihomo -t` 只检查它会检查的东西（例如 `listen` 写成无效地址时它也通过）。

**TUN**：给内核文件加 `CAP_NET_ADMIN` 和 `CAP_NET_BIND_SERVICE`（`setcap`），而不是用 root 运行。第一次开 TUN 时确认后，tabby 先用 `pkexec` 请求授权；没有图形认证代理（例如 Hyprland 下没有 polkit agent）时，临时退出全屏，在终端里执行 `sudo setcap …`，完成后回到界面。内核所在分区以 `nosuid` 挂载时文件能力不生效，tabby 会提前报错，请用 `[core].binary` 指向别处（`/tmp` 和一些外接盘常常是 `nosuid`）。升级或替换内核文件后能力会丢失，TUN 会被关掉并提示重新授权。

**登录时启动**：设置页打开后写入 `~/.config/systemd/user/tabby-mihomo-<后缀>.service` 并 `enable`（`WantedBy=default.target`），登录后由 systemd 直接拉起内核，不需要 tabby 在运行；关闭则 `disable` 并删除单元文件，正在运行的内核不受影响。这是登录启动，不是开机启动，tabby 不执行 `loginctl enable-linger`。没有 systemd 用户实例时该项置灰（`需要 systemd`）。

**系统代理**：设置页的 `系统代理` 把桌面的代理指向 `127.0.0.1:<Mixed 端口>`，关闭或停止内核时恢复成原来的值（记在 `system-proxy.toml`）。GNOME / MATE / Cinnamon 用 `gsettings`，KDE 用 `kwriteconfig6`（没有则 `kwriteconfig5`）；其他桌面（包括 Hyprland）该项置灰，显示“当前桌面不支持系统代理”。

**已知局限**

- 内核在 tabby 之外退出（例如被 `systemctl --user stop`）时，系统代理会仍然指向已关闭的端口；下次启动 tabby 时会提示，在设置页关闭系统代理即可恢复。
- 托管内核和别的内核（例如 FlClash）不要同时开 TUN 并 auto-route，它们会互相抢路由；也不要两个都用 7890。
- 临时单元随用户会话结束而停止；需要登录后自动运行请打开 `登录时启动`。
- 订阅里的 age 私钥是明文存放的；只支持 X25519 私钥，不支持口令加密和 SSH 密钥。
- 在 MATE 上 `gsettings` 的 schema 是 `org.gnome.system.proxy` 还是 `org.mate.system.proxy` 没有验证；GNOME / KDE 桌面本身没有在真实环境里验证，只验证了命令的参数和写入的值。
- 导入订阅时 `mihomo -t` 可能为 `GEOIP` / `GEOSITE` 规则下载 geo 数据（第一次较慢）。
- 在真实环境里验证过：systemd 单元、登录单元（`systemctl --user restart default.target`）、`setcap` 授权流程（用替身命令）、订阅导入与自动更新、age 解密、`gsettings`（内存后端）与 `kwriteconfig6`（临时目录）。**没有**验证真实的 `pkexec` / `sudo setcap`、TUN 设备与路由，以及注销再登录。

## 主题

| 主题                                                             | 说明                                   |
| ---------------------------------------------------------------- | -------------------------------------- |
| `terminal`（默认）                                               | 仅用 16 色 ANSI，跟随终端配色          |
| `tokyo-night` `catppuccin-mocha` `gruvbox-dark` `nord` `dracula` | 真彩深色，使用终端背景                 |
| `catppuccin-latte`                                               | 真彩浅色，自带背景，深色终端中同样清晰 |

`C` 打开主题弹窗，移动光标即时预览，`Enter` 选用并写回 `config.toml` 的 `theme` 行（仅改这一行，保留注释），`Esc` 取消。

### 自定义主题

在 `themes/` 目录（与 `config.toml` 同级）放置 `<name>.toml`，文件名即主题名：

```toml
extends = "tokyo-night"      # 可选，起点内置主题，默认 terminal

[colors]
accent = "#ff9e64"
surface = "#2a2f45"
bg = "#16161e"
```

优先级：内置主题 < 主题文件 `[colors]` < `[theme_overrides]`。`theme` 与 `--theme` 也可直接写文件路径（含 `/` 或以 `.toml` 结尾）。

### 颜色键

`[colors]` 与 `[theme_overrides]` 使用同一套键：

| 键                              | 用途                                                        |
| ------------------------------- | ----------------------------------------------------------- |
| `fg` / `muted`                  | 正文 / 次要文字与边框                                       |
| `bg`                            | 整页背景；`reset` 为终端背景                                |
| `surface` / `stripe`            | 光标行背景 / 表格隔行条纹（`reset` 关闭条纹）               |
| `accent`                        | 焦点、选中项、按键名                                        |
| `success` / `warning` / `error` | 正常、需注意、失败（同时用于延迟 <200 / 200–500 / >500 ms） |
| `c1` `c2` `c3` `c4`             | 上传、下载、连接数、内存                                    |

取值为字符串：`#rrggbb`；16 色 ANSI 名（`red`、`light-green`、`dark-gray` 等，小写）；或 `reset`（终端默认色，`surface` 不允许）。

### 图标

`icons = "nerd"` 使用 Nerd Font 图标，需终端字体打过 Nerd Fonts v3 补丁，推荐 **Nerd Font Mono** 变体（图标严格占一格）；显示错位时改回 `unicode`。

## 页面

| 页面        | 内容                                                                                             |
| ----------- | ------------------------------------------------------------------------------------------------ |
| 1 概览      | 近 60 秒流量图、代理模式、内核信息，以及上传 / 下载 / 连接数 / 内存卡片（高度 ≥ 28 行）          |
| 2 代理      | 代理组与节点网格；选择节点、单节点 / 整组测速、排序、过滤                                        |
| 3 Providers | 代理集合与规则集合；更新与健康检查                                                               |
| 4 规则      | 规则列表与过滤                                                                                   |
| 5 连接      | 实时连接表；详情、关闭、排序、暂停、过滤                                                         |
| 6 日志      | 内核日志流；按级别订阅、暂停、清空、过滤（保留 2000 条）                                         |
| 7 设置      | 模式、局域网连接、日志级别、IPv6、端口、TUN、重载配置、刷新 FakeIP、更新 GeoData、重启与升级内核；托管内核另有登录时启动、系统代理、DNS 覆盖、停止内核 |
| 8 订阅      | 托管内核的订阅：导入、切换、更新、自动更新与间隔、改名、更换 age 私钥、删除（连接外部控制器时只显示说明） |

控制器不可达时顶部出现重连横幅（1、2、4、8、16 秒，之后每 30 秒），`r` 立即重试，`P` 换配置；secret 错误同样会提示。

## 按键

`?` 打开帮助，内容为当前页面的实际按键。弹窗打开时按键只作用于弹窗，`Esc` 关闭；`Ctrl+C` 随时退出。过滤框与端口输入框内所有按键都是文字。

**全局**

| 按键                          | 作用                             |
| ----------------------------- | -------------------------------- |
| `1`–`8` / `Tab` / `Shift+Tab` | 跳转 / 下一页 / 上一页           |
| `r`                           | 刷新当前页，出错时重试           |
| `m`                           | 切换模式：rule → global → direct |
| `P` / `C`                     | 切换配置 / 主题                  |
| `?`                           | 帮助                             |
| `q` / `Ctrl+C`                | 退出                             |

**列表**

| 按键                | 作用                                                 |
| ------------------- | ---------------------------------------------------- |
| `j` `k` `↓` `↑`     | 上下移动                                             |
| `g` / `G`           | 顶部 / 底部（日志页：最早 / 最新并跟随）             |
| `Ctrl+d` / `Ctrl+u` | 翻半页                                               |
| `/`                 | 过滤（不区分大小写的子串）；`Enter` 保留，`Esc` 清除 |

**页面**

| 页面      | 按键                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------ |
| 代理      | `Enter` 选节点 · `h` `l` 左右移动 · `t` 测当前节点 · `T` 测整组（仅 mihomo）· `s` 排序（默认 → 延迟 → 名称） |
| Providers | `u` 更新当前项 · `U` 更新全部（需确认）· `h` 健康检查（代理集合）                                            |
| 连接      | `Enter` 详情 · `x` 关闭所选 · `X` 关闭全部（需确认）· `s` 切换排序列 · `S` 反转 · `p` 暂停                   |
| 日志      | `l` 切换订阅级别 · `p` 暂停 · `c` 清空                                                                       |
| 设置      | `Enter` / `Space` 切换或执行（端口行进入编辑）· `y` / `n` 确认 / 取消带 `!` 的操作                           |
| 订阅      | `Enter` 切换 · `a` 添加 · `u` 更新 · `U` 全部更新（需确认）· `t` 自动更新开关 · `d` 删除（需确认）           |

有过滤条件时，`U` 与 `X` 只作用于匹配项。

## 鼠标

默认启用，`mouse = false` 可关闭。

| 操作     | 效果                                               |
| -------- | -------------------------------------------------- |
| 左键单击 | 选中；标签页跳转；表头列排序；模式标签切换代理模式 |
| 左键双击 | 激活（同一行 400 ms 内连按两次），等同 `Enter`     |
| 滚轮     | 滚动指针下的列表；弹窗打开时滚动弹窗               |

## 内核支持

| 内核       | 支持情况                                                                                                                            |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| mihomo     | 全部功能                                                                                                                            |
| 原版 Clash | 通用功能可用；mihomo 独有项（TUN、内存、规则集合、整组测速、FakeIP 刷新、GeoData、重启与升级内核）显示“需要 mihomo”或置灰，不发请求 |

内核类型由 `GET /version` 的 `meta` 字段判断。原版 Clash 的行为由 `tests/fixtures` 中的样本与单元测试覆盖，未连接真实实例验证。

## 终端尺寸

| 条件                   | 变化                                     |
| ---------------------- | ---------------------------------------- |
| 宽 < 100 列            | 代理组折叠为顶部选择行；标签栏只显示数字 |
| 宽 < 70 列             | 连接表隐藏“进程”“总量”列；帮助改为单列   |
| 高 < 28 行             | 概览页隐藏统计卡片                       |
| 宽 < 50 列或高 < 15 行 | 仅提示“终端尺寸过小”，恢复后继续使用     |

## 日志

终端归 TUI 所有，tabby 自身日志写入 `$XDG_STATE_HOME/tabby/tabby.log`（默认 `~/.local/state/tabby/tabby.log`），追加写入，不轮转；panic 也记录于此。级别由 `RUST_LOG` 控制，默认 `info`：

```sh
RUST_LOG=tabby=debug tabby
```

这与“日志”页不同，后者显示的是内核日志。

## 开发

```sh
cargo build
cargo clippy --all-targets -- -D warnings
XDG_CONFIG_HOME="$(mktemp -d)" cargo test
```

测试请使用临时 `XDG_CONFIG_HOME`，避免触碰真实配置。设计与决策记录见 [`docs/plans/2026-11-05-tabby-tui.md`](docs/plans/2026-11-05-tabby-tui.md)。

发布包由 `make dist`（`scripts/dist.sh [<目标三元组>]`）生成到 `dist/`：两种 `tar.gz`、mihomo 的源码包 `mihomo-<tag>-source.tar.gz` 和 `SHA256SUMS`。随包的 mihomo 版本和两种架构的 sha256 固定在 `packaging/mihomo.env`，下载后不符就失败；amd64 用 `v1` 构建，任何 x86-64 CPU 都能运行（有新版本时 `升级内核` 按 CPU 选 v3/v2/v1）。依赖的许可证由 `cargo-about`（`cargo install --locked --features cli cargo-about`）生成 `THIRD-PARTY-LICENSES`。在同一台机器上，同一个提交打出的包逐字节相同（文件时间取最后一次提交的时间）。

## 许可证

[MIT](LICENSE)。mihomo 是独立程序，遵循它自己的许可证（GPL-3.0）：托管内核下载的、`PATH` 里的都是如此；发布包 `with-mihomo` 带的那份未经修改，它的许可证和源码说明在 `share/licenses/tabby/mihomo/`，源码包与发布包一起发布。Rust 依赖的许可证见发布包里的 `share/licenses/tabby/THIRD-PARTY-LICENSES`。

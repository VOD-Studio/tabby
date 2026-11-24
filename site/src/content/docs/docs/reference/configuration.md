---
title: 配置参考
description: config.toml、环境变量、主题和数据文件的完整参考。
---

## 文件位置

| 文件 | Linux 默认路径 | macOS 默认路径 |
| --- | --- | --- |
| 配置文件 | `~/.config/tabby/config.toml` | `~/Library/Application Support/tabby/config.toml` |
| 自定义主题 | `~/.config/tabby/themes/` | `~/Library/Application Support/tabby/themes/` |
| Tabby 日志 | `~/.local/state/tabby/tabby.log` | `~/Library/Application Support/tabby/tabby.log` |
| 托管数据 | `~/.local/share/tabby/core/` | `~/Library/Application Support/tabby/core/` |

绝对路径形式的 `XDG_CONFIG_HOME`、`XDG_STATE_HOME` 和 `XDG_DATA_HOME` 可分别重定向配置、状态和数据。Tabby 在这些路径下使用 `tabby/` 子目录。macOS 未指定状态目录时，状态文件随数据目录一起移动。托管内核的进程日志位置取决于[启动方式](../../troubleshooting/#去哪里看日志)。

## 完整示例

```toml
default = "local"
theme = "terminal"
icons = "unicode"
mouse = true

[delay]
url = "https://www.gstatic.com/generate_204"
timeout_ms = 5000

[profiles.local]
url = "http://127.0.0.1:9090"
# secret = "your-controller-secret"

[profiles.lab]
url = "https://mihomo.example.net"

[theme_overrides]
accent = "#c56e4e"

# 需要托管时取消 [core] 的注释。binary 可省略
# [core]
# binary = "/absolute/path/to/mihomo"
```

没有配置文件时，Tabby 使用内置默认值。配置文件中的 TOML 格式有误时，Tabby 会在进入界面前报错。`default` 指定默认启动配置。添加 `[core]` 后可选用保留配置名 `core` 来托管内核。`[profiles.core]` 会导致配置报错。

控制器密钥若写在 `profiles` 中，将以明文存储。避免提交真实配置，限制文件的访问权限。命令行参数会出现在进程列表中，临时密钥可通过 `TABBY_SECRET` 输入。

## 命令行和环境变量

各项覆盖值的优先级为 **CLI 参数 → 环境变量 → `config.toml` → 内置默认值**。空的覆盖值按未设处理。`--url` 和 `--secret` 分别覆盖启动配置中的对应值。用 `P` 切到某个已保存的配置时会使用该配置自己的地址和密钥。

| 参数 | 环境变量 | 含义 |
| --- | --- | --- |
| `--profile` | `TABBY_PROFILE` | 选择配置名，优先于上次选择和文件中的 `default` |
| `--url` | `TABBY_URL` | 指定外部控制器，默认 `http://127.0.0.1:9090` |
| `--secret` | `TABBY_SECRET` | 外部控制器的密钥 |
| `--theme` | `TABBY_THEME` | 本次运行的主题，优先于已保存的主题。运行时按 `C` 切换的主题只在本次会话生效 |

用 `tabby --help` 查看参数，用 `tabby --version` 查看版本和可用的构建信息。

### 恢复上次选中的控制器

按 `P` 并确认切换后，Tabby 将选择记在状态目录的 `last-controller.toml`。下次启动时，若没有通过命令行或环境变量指定 `profile`、`url` 或 `secret`，就优先恢复上次选择，并读取该配置当前的地址和密钥。该配置改名或删除后，回退到文件里的 `default` 或内置地址。「启动参数指定」一项仅在本次会话可用。用 `tabby --profile core` 可以直接启动托管内核。

## 测速参数

`[delay].url` 是通过节点访问的测速 URL，默认是 `https://www.gstatic.com/generate_204`。`timeout_ms` 默认 `5000`，单位是毫秒。遇到所有节点都超时时，可换成这些节点能够访问的 URL 再测。

## 主题和交互

按 `C` 预览并选择主题。内置主题标识为 `terminal`、`tokyo-night`、`catppuccin-mocha`、`catppuccin-latte`、`gruvbox-dark`、`nord`、`dracula`、`clay-dark` 和 `clay-light`。

自定义主题例如 `themes/my-tabby.toml`。

```toml
extends = "tokyo-night"

[colors]
accent = "#ff9e64"
bg = "#16161e"
```

用 `theme = "my-tabby"` 选中该文件。也可以直接指定主题文件路径。相对路径在 `config.toml` 中以配置目录为准，在 `--theme` 或 `TABBY_THEME` 中以当前工作目录为准。颜色支持 `#rrggbb`、16 色 ANSI 名称和 `reset`。`[theme_overrides]` 的值会覆盖当前主题。终端使用 Nerd Font v3 时，可以把 `icons` 设为 `"nerd"`。`mouse = false` 让终端保留原生的鼠标文本选择。

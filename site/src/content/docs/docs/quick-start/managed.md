---
title: 托管 mihomo
description: 让 Tabby 启动 mihomo，导入第一份订阅并确认代理是否可用。
---

Tabby 的托管模式可以启动、停止和管理自己的 mihomo。已有其他客户端时，先确认 Mixed 端口可用，并让同一台机器的 TUN 路由由一套内核负责。

## 1. 启用托管模式

在 Linux 的 `~/.config/tabby/config.toml` 或 macOS 的 `~/Library/Application Support/tabby/config.toml` 添加以下内容。若设置了绝对路径 `XDG_CONFIG_HOME`，使用其中的 `tabby/config.toml`。已有文件时，在原有 TOML 对应位置修改，避免重复定义 `default` 或 `[core]`。

```toml
default = "core"

[core]
# binary = "/绝对路径/mihomo"
```

## 2. 启动内核

运行 `tabby --profile core`。Tabby 会接入已在运行的托管内核，或寻找并启动可用的 `mihomo`。找不到时，按界面提示确认下载。启动成功后，概览会显示内核版本和代理模式。没有启用订阅时使用内置的直连配置，Mixed 端口默认为 `7890`。

## 3. 导入并切换订阅

1. 按 `8` 进入「订阅」，按 `a`，输入订阅 URL 或本地文件路径。
2. 按 `Enter` 提交。如果导入 age 加密订阅，按提示输入 X25519 私钥。
3. 第一份订阅导入成功后会自动启用。之后要换订阅，选中目标项并按 `Enter`。确认使用标记后，按 `2` 核对代理组和节点。

先检查内核的本地 Mixed 端口。若默认端口已改，请替换下面的 `7890`。

```sh
curl --noproxy '' --proxy http://127.0.0.1:7890 --head https://example.com/
```

收到 HTTP 响应说明请求经过了本地代理端口。实际去向取决于当前模式、规则和节点。需要停止内核时，在「设置」页选择「停止内核」并确认。退出 TUI 后，托管内核仍可在后台提供服务。

更多操作见[内核生命周期](../../manage/kernel/)和[订阅管理](../../manage/subscriptions/)。

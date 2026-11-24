---
title: Tabby 文档
description: 在终端里连接或托管 mihomo。
---

Tabby 是适用于 Linux 和 macOS 的中文 mihomo 终端控制台。已经在运行 mihomo 的用户可以连接外部控制器。需要 Tabby 安装和管理内核的用户，可以启用托管模式。

本手册以稳定版 **v0.3.1** 为准。

## 连接已有内核

确认 mihomo 已启用外部控制器，再用 `tabby --url http://127.0.0.1:9090` 连接。这里的地址是示例，使用带密钥的控制器时，在启动前设置 `TABBY_SECRET`。

## 托管 mihomo

在 `config.toml` 设置 `default = "core"` 并添加 `[core]` 表。启动 Tabby 后，进入「订阅」页按 `a` 导入 URL 或文件，再按 `Enter` 切换。

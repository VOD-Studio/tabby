---
title: DNS 覆盖
description: 用托管设置调整 mihomo 的 DNS 参数并恢复订阅默认值。
---

在「设置 → 托管内核 → DNS 覆盖」打开编辑项。覆盖值按键合并到当前订阅的 `dns` 段。未设置的键继续采用订阅自身的值。

## 可以编辑的字段

| 类型 | 字段 |
| --- | --- |
| 开关 | `ipv6`、`prefer-h3` |
| 选项 | `enhanced-mode`、`fake-ip-filter-mode` |
| 文本 | `listen` |
| 列表 | `fake-ip-filter`、`default-nameserver`、`nameserver`、`fallback`、`proxy-server-nameserver`、`direct-nameserver` |

`enhanced-mode` 可选 `fake-ip` 和 `redir-host`。`fake-ip-filter-mode` 可选 `blacklist`、`whitelist` 和 `rule`。修改模式或名称服务器前，先确认当前订阅的规则与 DNS 用法。

## 修改和恢复

选中字段，按 `Enter` 打开或修改，然后按编辑提示保存。Tabby 会用 `mihomo -t` 验证新配置。验证出错时按提示修正输入，原值仍可继续使用。

需要回到订阅默认值时，在弹窗内按提示清掉该项覆盖。保存后用当前代理方式重新访问一个域名，并在[内核日志](../../use/logs/)检查 DNS 相关错误。

---
title: 平台与内核兼容性
description: Linux、macOS、mihomo 和原版 Clash 的支持区别。
---

## 系统

Linux 支持 x86_64 和 aarch64，预编译发布包要求 glibc 2.35 或更高版本。macOS 支持 Apple Silicon 和 Intel，最低系统版本为 macOS 12。完整安装方式见[安装](../../install/)。

## 内核

| 功能 | mihomo | 原版 Clash |
| --- | --- | --- |
| 已有外部控制器 | 推荐 | 部分支持 |
| 代理切换、单节点测速、基础规则、流量、连接和日志 | 支持 | 可用范围取决于控制器返回的数据 |
| 整组测速、规则 Providers 和 mihomo 专属维护 | 支持 | 界面会禁用不支持的操作 |
| 内存统计和连接额外字段 | 取决于内核返回 | 可能缺失 |
| Tabby 托管、订阅、DNS 和系统集成 | 托管模式支持 | 托管模式使用 mihomo |

原版 Clash 的兼容性由样本和单元测试覆盖。遇到字段缺失或操作被禁用，先确认当前控制器的内核和版本。平台权限和运行限制见[系统集成](../../manage/system-integration/)。

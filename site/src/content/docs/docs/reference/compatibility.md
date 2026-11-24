---
title: 平台与内核兼容性
description: Linux、macOS、mihomo 和原版 Clash 的支持区别。
---

## 系统

Linux 支持 x86_64 和 aarch64，预编译发布包要求 glibc 2.35 或更高版本。macOS 支持 Apple Silicon 和 Intel，最低系统版本为 macOS 12。完整安装方式见[安装](../../install/)。

## 内核

| 功能 | mihomo | 原版 Clash |
| --- | --- | --- |
| 代理切换、单节点测速、代理 Providers、规则、流量、连接和日志 | 支持 | 支持基础功能，具体字段取决于内核 |
| 整组测速、规则 Providers、TUN 设置和 mihomo 专属维护 | 支持 | 不可用 |
| 内存统计 | 支持 | 不可用 |
| Tabby 托管、订阅、DNS 覆盖和系统集成 | 托管模式支持 | 不可用 |

原版 Clash 的兼容性由样本和单元测试覆盖。遇到字段缺失或操作被禁用，先确认当前控制器的内核和版本。平台权限和运行限制见[系统集成](../../manage/system-integration/)。

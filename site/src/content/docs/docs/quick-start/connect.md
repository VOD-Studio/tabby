---
title: 连接已有内核
description: 将 Tabby 连接到正在运行的 mihomo 外部控制器。
---

准备一套正在运行的 mihomo，并查看配置里的 **外部控制器**地址。Tabby 的默认地址是 `http://127.0.0.1:9090`。应用使用的 Mixed 端口通常是 `7890`，具体以你的内核配置为准。

## 1. 确认控制器监听地址

例如，本机 mihomo 配置可以包含以下两项。已有有效配置时，直接使用其中的地址和密钥。

```yaml
external-controller: 127.0.0.1:9090
secret: "your-controller-secret"
```

配置改动要由运行 mihomo 的客户端重新加载。连接远程控制器时，优先使用已有的可信内网、HTTPS 或 SSH 转发。控制器密钥可执行管理操作，避免将控制器直接暴露给公网。

## 2. 启动 Tabby

控制器允许本机无密钥访问时，可直接运行。

```sh
tabby --url http://127.0.0.1:9090
```

控制器要求密钥时，设置 `TABBY_SECRET`。下面的命令适用于 Bash 或 Zsh，输入时终端会隐藏内容。

```sh
read -rs TABBY_SECRET
export TABBY_SECRET
tabby --url http://127.0.0.1:9090
unset TABBY_SECRET
```

连接控制器时，Tabby 会忽略 `HTTP_PROXY` 等代理环境变量。

## 3. 确认连接和常用操作

概览显示内核版本、代理模式及实时统计。按 `2` 打开代理页，按 `?` 查看当前页面按键。空闲内核的流量和连接数可以为零。

经常连接多个控制器时，可在 `config.toml` 为每个控制器添加 `[profiles.<名称>]`。用 `P` 在界面内切换，或用 `tabby --profile <名称>` 指定启动配置。配置路径、默认选择和示例见[配置参考](../../reference/configuration/)。

若概览提示连接失败，确认地址指向外部控制器、检查密钥，然后按 `r` 重试。参见[故障排查](../../troubleshooting/)。

# Steam Frame 6 GHz 设置工具

用于查看 Steam Frame USB 适配器的国家码和 6 GHz 状态，并将运行地区设置为 US。

不涉及驱动逆向，也不会修改驱动文件，仅适用于 Windows，Linux可以直接执行 `sudo iw reg set US`。

## 使用

1. 插入适配器，以管理员身份运行 `steam-frame-6ghz-tool.exe`。
2. 只有一个匹配设备时自动选中并查询状态；多个设备时，选择后自动查询。
3. 点击“设置 US”，确认后执行一次并自动复查。

> [!TIP]
> 如果软件上国家码已经是US，头显依旧一直配对不上，可参考：[Windows 已设为 US 仍无法连接](#windows-已设为-us-仍无法连接-frame)

## 自动应用

> 由于电脑重启或适配器重新插拔后设置可能失效，需要再次操作。若不想每次手动设置，可以开启“自动应用”。  

以管理员身份运行程序，点击“自动应用”旁的“开启”并确认。开启后，系统会在开机或适配器重新插入时自动检查，并按需设置为 US；处理完成后自动退出，不会常驻后台。  

- 自动日志保存在程序旁的 `logs` 目录，程序移动后需重新开启自动服务。
- 日志最多保留 20 份，不包含 Wi-Fi 密码。
- 自动执行异常时会暂停，不会自动重试。
- 卸载服务不会删除日志，也不会恢复当前 US 状态。
- 程序更新后，需重新安装自动服务。
- 未验证驱动版本会尝试执行，但不保证有效。

## Windows 已设为 US 仍无法连接 Frame

通过 SSH 连接到 Frame，或直接在 Frame 中打开终端。推荐使用 SSH，操作会更方便，可参考 [SSH 开启方法](#frame-如何开启ssh)。

先执行：

```sh
iw reg get
```

检查当前无线区域是否为 `CN`。

如果显示为 `CN`，可以执行：

```sh
sudo iw reg set US
```

然后重新尝试配对。

需要注意的是，这种方式在设备重启后可能会失效。如果希望重启后仍保持该设置，可以修改无线区域配置文件：

```sh
sudo sed -i 's/^#WIRELESS_REGDOM="US"$/WIRELESS_REGDOM="US"/' /etc/conf.d/wireless-regdom
grep -n '^WIRELESS_REGDOM=' /etc/conf.d/wireless-regdom
```

确认 `grep` **只输出一条**：

```text
WIRELESS_REGDOM="US"
```

然后重启设备：

```sh
sudo reboot
```

重启完成后，再执行：

```sh
iw reg get
```

确认全局区域以及 `phy#0` 均显示为 `US`，随后重新进行配对。

如需恢复原配置，执行：

```sh
sudo sed -i 's/^WIRELESS_REGDOM="US"$/#WIRELESS_REGDOM="US"/' /etc/conf.d/wireless-regdom
sudo reboot
```

> 这里只提供其中一种实现方式。Linux 下可行的方法很多，熟悉相关操作的朋友也可以自行尝试其他方案。


## Frame 如何开启SSH  

1. 首先在头显中开启**开发者模式**。
2. 然后进入**开发者设置**，滑到页面底部并设置登录密码。

密码设置完成后，就可以通过 SSH 连接 Steam Frame：

`steamos@frame`

或者使用设备的 IP 地址：

`steamos@<设备IP>`



## 注意

- 当前程序仅在有限环境下测试，尚未经过广泛验证，不保证在所有设备、系统及驱动版本上正常工作。
- 支持 Windows 10/11 x64。已验证原厂驱动 `5.32.908.2026`；其他版本仅提示未验证，不限制操作。
- 设置仅影响运行时状态，不会永久写入适配器；未开启自动应用时，重启或重新插拔后可能需要再次设置。
- 使用时关闭其他适配器诊断工具。操作失败或结果不确定时，先查看日志，稍后刷新查询。
- 仅用于授权的屏蔽实验环境。设置 US 会改变无线运行策略，请遵守所在地无线电规定。


## 构建

安装 Rust MSVC 工具链、Visual Studio C++ Build Tools 和 Windows SDK 后执行：

```powershell
cargo test --locked
cargo build --release --locked
```

生成文件：`target/release/steam-frame-6ghz-tool.exe`。

## 发布

推送与 `Cargo.toml` 版本一致的标签后，GitHub Actions 会自动测试、构建并发布 Release，附带 Windows x64 程序和 SHA256 校验文件。

```powershell
git tag v0.1.0
git push origin v0.1.0
```

也可在 Actions → Build and release → Run workflow 中选择 `main`，填写已有标签（如 `v0.1.0-preview.1`）手动发布。构建使用该标签的代码。

# UU 客户端的 DTLS 接收竞态补丁

基础是 crates.io 的 `webrtc-dtls 0.10.0` 原始发布源码，MIT OR Apache-2.0，保留原许可证。
注册表压缩包 SHA256：`86e5eedbb0375aa04da93fc3a189b49ed3ed9ee844b6997d5aade14fc3e2c26e`。
原始源码记录的 upstream git：`d1778a7323ffe3becb84e13fd8a7bf5d4758a5f0`，`dtls` 子目录。

## 原因与修改

`ConnReaderContext`把握手记录排入`handshake_tx`后，会在内部循环等待该次通知的确认。
握手状态机完成时设置成功标记并关闭`handshake_done_tx`，随后返回；其结构体仍持有通知接收端，
所以已经排队的确认发送端也可能一直保留。外层select虽然监听握手结束，已经进入的内部循环却没有监听，
导致唯一的DTLS接收任务永久等待，不再递交加密应用数据。SRTP走另一条接收路径，仍能更新视频；
SCTP没有收到握手包，状态就一直停在connecting。

本补丁在内部等待中也观察握手结束，退出过期等待；外层在结束和通知同时就绪时优先处理结束。
不改握手认证、证书指纹、加密、重放检查或SCTP协议，也不依赖延长超时与自动重连。

线索来自只读参考项目`/home/yfy/repo/uu-remote-linux`的
`502635b156db334e412862668b7f9e79d604a9e6`版本DTLS补丁。这里以注册表0.10.0为基础适配，
没有复制其整个GPL项目、旧rustls依赖或额外日志改动；本回归用例为本项目编写。

## 验证

`src/conn/completion_race_tests.rs`用通道屏障强制通知先成功、握手随后结束、确认发送端仍存活的顺序，
接着发送真实AES-GCM加密的合成应用记录，检查DTLS是否继续解密和投递。
它不靠随机丢包、真实桌面、账号或实际键盘事件。

- 原版代码：300ms内无法投递，失败于`completed DTLS must continue reading SCTP`。
- 修改后：同一测试立即通过，正常双端DTLS收发/关闭用例也通过。
- 本项目迟到ICE回环继续验证CONTROL设置、输入心跳、鼠标与键盘共同送达。
- 2026-10-19，交付Windows源版本`f74fc9d`后，用户明确回复“恢复了”，确认本次键鼠恢复，见交接17。

vendored crate排除在workspace成员外，生产版本使用仓库根Cargo.lock；其单独Cargo.lock仅锁定上游测试依赖。

```sh
cargo test --manifest-path vendor/webrtc-dtls/Cargo.toml --lib post_handshake_completion --target-dir target --locked
cargo test --manifest-path vendor/webrtc-dtls/Cargo.toml --lib test_routine_leak_on_close --target-dir target --locked
cargo test -p uu-core --lib startup_tests --locked
cargo tree -i webrtc-dtls --locked
```

功能修改仅在原始`src/conn/mod.rs`，另加回归测试文件；`conn_test.rs`只清理原始发布包的一处行尾空格。后续升级应先比较上游修复，能移除patch时移除，
不能无条件覆盖整个WebRTC依赖。Windows打包检查包含vendor，确保清单提交号确实对应构建代码。

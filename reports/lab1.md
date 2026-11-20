# Lab 1（第 3 章）：用户态程序、系统调用与可观测的内核

## 我要解决的问题

第 3 章的内核要把「一批静态编译进内核镜像的用户程序」跑起来，并让它们在同一个
地址空间里协作式轮转；同时给 `user/` 侧的三个新用例（`ch3_sleep`、`ch3_sleep1`、
`ch3_trace`）提供真实可用的 `sys_get_time` / `sys_sleep` / `sys_trace`。
判分口径不是我说了算：`make test CHAPTER=3` 之后由 `check/ch3.py` 对内核串口输出做
7 条正则匹配，必须 7/7。

## 内核实际做了什么

**应用加载**（`os/src/loader.rs`）。13 个用户程序以 `.incbin` 的形式被打进内核 data 段，
`build.rs` 扫描 `user/build/bin/` 生成 `src/link_app.S` 与 `_num_app`。`load_apps()` 把每个
程序复制到 `APP_BASE_ADDRESS + i * APP_SIZE_LIMIT`（`0x80400000`，每槽 `0x20000`）并清零整槽，
最后执行 `fence.i` —— 这一步是必需的：RISC-V 不保证指令 fetch 能看到刚写入的指令内存。

**每任务的地址可见范围**。除了应用槽本身，第 3 章给每个任务分配的用户栈
（`USER_STACK[i]`，4 KiB）是内核镜像里的静态数组，**不在应用槽区间内**。
`loader.rs` 因此同时导出 `app_region(app_id)` 与 `user_stack_region(app_id)`，
`task::current_user_ranges()` 把两者一起交给 `syscall::user_addr_valid()` 判定。

**系统调用分发**（`os/src/syscall/mod.rs`）。`SYSCALL_WRITE/EXIT/YIELD/GET_TIME/TRACE`
五条真实实现；未知调用号只打印错误并让该任务退出，不 panic —— 因为 panic 会连带打死
其它任务的输出，把一次应用侧的 bug 放大成整轮 0 分。

**`sys_get_time`**：读内核累计微秒数，写入用户 `TimeVal{sec,usec}`；指针非法返回 `-1`。

**`sys_sleep`**：基于 `get_time_ms()` 的轮询式睡眠（依赖 `sys_yield` 让出），
供 `ch3_sleep1` 打印 `time_msec = ... after sleeping ... ticks, delta = ...ms`。

**`sys_trace`** 三种请求：
- `TRACE_READ` / `TRACE_WRITE`：经地址闸门后读写应用自己的一个字节，越界一律 `-1`，
  内核绝不替应用解引用可疑指针；
- `TRACE_SYSCALL`：返回**当前任务**调用某个 syscall 的累计次数。计数点在 trap handler
  **派发之前**（`task::record_syscall`），所以「查询某调用次数」这一次 `sys_trace` 自身
  也被计入 —— 这正是 `ch3_trace.rs` 注释「注意这次 sys_trace 调用本身也计入」要求的语义：
  该用例里第一次断言 `count_syscall(SYSCALL_TRACE) == 2` 时才发生了 2 次 trace 调用
  （第 1 次查 `GETTIMEOFDAY`，第 2 次就是这次自查），第二次断言 `== 7` 也依赖同一规则。
  未知调用号返回 `None`/`-1`，避免把「没实现」误报成「没调用过」。

## 怎么验证的

本地复刻了官方 CI，而不是自己另造一把尺子：pin 到 `rCore-Tutorial-Checker`
`7d61ec55b58eed6ca7052917846c1b87af34563a`、`rCore-Tutorial-Test`
`a0593662ad55d670ba8c27ce1763347cd0dd552f`，用学生仓库 `rust-toolchain.toml` 指定的
`nightly-2024-05-02` 工具链 + `qemu-system-riscv64` 跑真实的 `make test CHAPTER=3`
（官方 Makefile 用 `timeout --foreground 30s` 截串口输出）。学生侧 token 为空 ⇒
`BASE=2` 且 `BASE_CHAR` 为空，所以实际执行的是 `check/ch3.py`，分母 7。

判分器自己会把每条 pattern 的判决打进日志（`[PASS] found <...>` / `[FAIL] not found <...>`），
所以缺哪几条不需要猜。

## 结果

```
Test passed6382: 7/7
```

日志：`/Users/eieieiei/Documents/Qoder/2026-11-20/91507e43/oscamp/logs/c_ch3_fix3.log`
（内核 ELF 688240 字节，`rustc 1.80.0-nightly (c987ad527 2024-05-01)`，QEMU 11.1.3）

## 过程中真实踩到的两个坑

1. **`#![deny(warnings)]` 把警告变成致命错误。** 一次重构把 `TrapContext` 拆到
   `os/src/trap/context.rs`，`trap/mod.rs` 忘了重导出，且 `sip` CSR 改用裸 asm 后导入变成
   unused ⇒ `error: could not compile os`。整章从 5/7 直接掉到 0/7。
2. **地址闸门漏了用户栈。** `sys_get_time` 的目标 `TimeVal` 是栈上局部变量，
   闸门只认应用槽 ⇒ 合法指针被判 `-1` ⇒ 用户程序
   `Panicked at src/bin/ch3_sleep.rs:16, assertion failed: current_time > 0`，串口上留下
   `current time_msec = -1`，`sleep`/`sleep1` 五条 EXPECTED 一起丢。
   修法就是上面那对 `app_region` / `user_stack_region`。
   （`sys_write` 当时仍然「正常」，因为打印用的字符串常量在应用 `.rodata` 里，
   恰好在应用槽内 —— 这解释了为什么故障只体现在时间类用例上。）

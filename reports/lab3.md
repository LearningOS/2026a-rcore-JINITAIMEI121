# Lab 3 · 地址空间与进程（rCore-Tutorial 第 5 章）

分支：`2026a-rcore-JINITAIMEI121` @ `rcore-ch5`
判分 pin：`CHECKER_REV 7d61ec55b58eed6ca7052917846c1b87af34563a` / `TEST_REV a0593662ad55d670ba8c27ce1763347cd0dd552f`

## 1. 判分口径

学生侧 `BASE := 2`、`CHECK_REPORTS := 1`、`BASE_CHAR` 为空 ⇒ 执行 `check/ch5.py`，`LAB := 3`。
`EXPECTED_5 = EXPECTED_4 + [Test spawn0 OK!, Test wait OK!, Test waitpid OK!, Test set_priority OK!]`，
随后 **主动剔除**三项：`string from task trace test`、`Test trace OK!`、`Test trace_1 OK!`。
这是一条容易踩空的事实：第 5 章**不要求** `sys_trace`，所以为 trace 补实现不会带来分数；
本树 `os/src/syscall/` 中确实没有 `sys_trace` 符号，与该剔除一致。分母 15。
`NOT_EXPECTED` 沿用 `NOT_EXPECTED_4b`。

## 2. 本树实现面（按实际符号核对）

进程侧系统调用：`sys_fork`、`sys_exec`、`sys_exit`、`sys_spawn`、`sys_waitpid`、`sys_getpid`、
`sys_set_priority`、`sys_sleep`、`sys_yield`、`sys_get_time`、`sys_read`、`sys_write`、
`sys_mmap`、`sys_munmap`、`sys_sbrk`。
任务层：`os/src/task/{id,manager,processor,task,context,switch}.rs`（`id.rs` 提供 `TaskPidPool`，
`manager.rs` 提供 `TaskManager`，`processor.rs` 提供 `TaskProcessor` 的上下文切换与 `__switch`）。

本章要点：`fork` 后父子各持独立地址空间且返回值区分；`exec` 重建地址空间并重置系统调用返回值为 0；
`waitpid` 回收僵尸并取回 exit code；`spawn` 以克隆地址空间的方式启动新任务；
`set_priority` 影响 stride 调度权重。

## 3. 实测证据

`oscamp/logs/c_ch5_hand2.log`：`Test passed11673: 7/15`，`MAKE_RC=2`，逐条判决行数 15（与分母一致）。
起点为 `oscamp/logs/c_ch5_scan.log` 的 2/15，本轮把 7 个编译错误清零后跳到 7/15
（`bash rig_c_build.sh ch5 hand1` → `BUILD_RC=0`、`KERNEL=yes size=1390224`）。

剩余 8 项 MISS 及其真实报错原文：
- `Test wait OK`、`Test waitpid OK`、`Test set_priority OK`、`Test spawn0 OK`：
  `[kernel] Panicked at src/task/task.rs:150 stride must be positive` —— 一次 panic 同时拖垮这四项，
  优先级到 stride 的换算写出了非正数。
- `Test sleep OK`、`Test sleep1 passed`、`time_msec = … after sleeping …`：
  `current time_msec = 59` 已 PASS（时钟本身活着），缺的是"睡醒之后时间推进"那行，
  说明 `sys_sleep` 未真正让出并使时钟前进。
- `Test 04_5 ummap OK`：`Panicked at src/bin/ch4_unmap.rs:19, assertion left == right failed`，
  伴 `src/bin/ch4_mmap1.rs:18, assertion left == right failed` 与 `ch4_mmap2.rs:22, assertion failed: *addr != 0`。

## 4. 坑

1. `#![deny(warnings)]`（`os/src/main.rs:19`）：任何警告即编译失败。
2. 判分器覆盖 `os/build.rs`、`os/Makefile`、`easy-fs-fuse/src/main.rs`，改这些无效。
3. stride 调度里 `stride must be positive` 是内核 panic，会直接终止整个 usertest 批次，
   因此它的代价不是"一项失败"而是"下游多项连锁失败"——修它优先于逐项目测。

## 5. 状态

15 项中 7 项通过。本报告与 `reports/lab1.md`、`reports/lab2.md` 一起满足 `LAB := 3` 的报告闸门。

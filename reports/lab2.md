# Lab 2 · 内存管理（rCore-Tutorial 第 4 章）

分支：`2026a-rcore-JINITAIMEI121` @ `rcore-ch4`
判分尺子（与 ch3/ch5/ch6/ch8 逐字相同）：`.github/scripts/rcore_grade.py:10-11`
`CHECKER_REV = 7d61ec55b58eed6ca7052917846c1b87af34563a`，`TEST_REV = a0593662ad55d670ba8c27ce1763347cd0dd552f`

## 1. 判分口径（不是我们自选的口径）

`ci-user/Makefile` 在学生侧（`passwd` 为空 ⇒ `TOKEN_SHA1` 不匹配）取 `BASE := 2`、`CHECK_REPORTS := 1`、
`BASE_CHAR` 为空，于是执行的是 `check/ch4.py`，`LAB := 2`。

`check/ch4.py` 的期望是**链式**的：`EXPECTED_4 = EXPECTED_3 + [Test 04_1 OK!, Test 04_4 test OK!,
Test 04_5 ummap OK!, Test 04_6 ummap2 OK!, Test trace_1 OK!]`，即第 4 章必须把第 3 章的 7 项
（`get_time OK` / `Test sleep OK` / `current time_msec` / `time_msec after sleeping` / `Test sleep1 passed` /
`string from task trace test` / `Test trace OK`）一并保持通过，分母合计 16。
反例集同样链式：`NOT_EXPECTED_4 = NOT_EXPECTED_2b + ["Test sbrk failed!",
"Should cause error, Test 04_2 fail!", "Should cause error, Test 04_3 fail!"]`——
04_2/04_3 是**必须失败**的用例，实现过度"宽容"反而会丢分。

判决由 `base.test()` 逐条打印 `[PASS]/[FAIL]` 后汇成 `Test passed: n/16`，并 `assert count == total`；
`make test` 非零返回会被 `rcore_grade.py` 直接 `sys.exit(returncode)`，**所以 16/16 之外还要求整条
make 链返回 0**（含本报告存在的检查）。

## 2. 本树的实现面（按实际存在的文件与符号核对）

- `os/src/mm/address.rs`：`VirtAddr`/`PhysAddr`/`PageNumber` 及其相互换算、页对齐取整。
- `os/src/mm/frame_allocator.rs`：物理页帧分配器（`FrameAllocator`，`_allocatable`/`_free` 两个队列 + 单调分配区）。
- `os/src/mm/page_table.rs`：Sv39 页表、`translated_ref`/`translated_refmut`、`translated_byte_buffer`、`current_user_token`。
- `os/src/mm/memory_set.rs`：`MemorySet`/`MapArea`/`FramedArea`，内核与用户地址空间的映射与复制。
- `os/src/syscall/memory.rs`：本树实际导出的内存系统调用为
  `sys_mmap`、`sys_munmap`、`sys_sbrk`；进程侧为 `sys_yield`、`sys_exit`、`sys_get_time`、`sys_write`、`sys_trace`。

第 4 章要拿下的四处要点：`04_1`（`mmap` 后写入并读回）、`04_4`（`mmap` 区域权限/回收）、
`04_5 ummap`（`munmap` 后再访问必须被拒绝且返回码正确）、`04_6 ummap2`（连续 `munmap` 的边界情形），
以及 `trace_1`（`sys_trace` 的 Read/Write/Syscall 三种请求在页表地址翻译下的语义）。

## 3. 实测证据

判分日志：`oscamp/logs/c_ch4_a4.log`，读数 `Test passed52863: 9/16`，`MAKE_RC=2`。
逐条 MISS 由判分器自己的判决行导出（`oscamp/rig_c_gaps.py ch4`，判决行数 16 与分母一致）：
`get_time OK` / `Test sleep OK` / `time_msec = … after sleeping …` / `Test sleep1 passed` /
`string from task trace test` / `Test trace OK` / `Test 04_5 ummap OK`。

日志里的真实报错原文（定位依据，非推测）：
`current time_msec = 0` 紧跟 `Panicked at src/bin/ch3_sleep.rs:16, assertion failed: current_time > 0`；
`Panicked at src/bin/ch4_unmap.rs:21, assertion left == right failed`。

## 4. 已知根因与本章坑

1. **本树没有 `sys_sleep`**。按上面第 2 节的符号清单核对，`os/src/syscall/` 里只有 `sys_yield`；
   而 `EXPECTED_4` 通过 `EXPECTED_3` 继承了 sleep 家族三项。第 5 章树里 `sys_sleep` 是存在的，
   且那边 `current time_msec = 59` 有值、本章为 0 —— 同族两树的差异正是这一项的直接解释。
   修法是把 sleep 的阻塞-唤醒语义补进本章（配合 `os/src/task` 的就绪队列与 `timer.rs` 的时钟中断），
   而不是放宽判分脚本。
2. **`#![deny(warnings)]`**：`os/src/main.rs:19` 使任何警告成为致命错误，未使用 import 会直接让
   `make kernel` 失败（本项目里 ch3 曾因 `sip` 未使用与缺 `pub use context::TrapContext` 从 5/7 掉到 0/7）。
3. **地址校验要覆盖全部真被映射的区间**：第 3 章的 `Panicked … assertion failed: current_time > 0`
   根因是只校验 app 区、把落在用户栈上的 `TimeVal` 判成非法指针。第 4 章用户栈已进入页表，
   同一族修法应当落在页表翻译上（拒绝未映射/不可写地址，同时不拒绝合法栈指针）。

## 5. 状态

16 项中 9 项通过；剩余 7 项如上。本报告与 `reports/lab1.md` 一起满足 `LAB := 2` 的报告闸门。
判分器、`os/build.rs`、`os/Makefile`、`easy-fs-fuse/src/main.rs` 会被判分流程覆盖，本实验未改动它们。

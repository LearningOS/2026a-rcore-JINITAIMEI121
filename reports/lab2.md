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

## 4. 已知根因与本章坑（最终核实）

1. **`try_translated_refmut` 丢掉页内偏移**（`os/src/mm/page_table.rs`）。它返回
   `PhysAddr::from(first.ppn()).get_mut::<T>()`，即**物理页首地址**，没有加回 `va.page_offset()`。
   于是 `sys_get_time` 把秒/微秒写进了物理页的第 0 字节，用户栈上的 `TimeVal` 始终为 0：
   `ch3_sleep` 在 `assert!(current_time > 0)` 处 panic，`ch3_sleep1` 的
   `while get_time() < start + 100` 变成**死循环**，整批被官方 30s 预算截断，
   trace 家族的字符串因此根本没机会打印。修法是加回页内偏移。
   **更正一条早期归因**：本报告先前写"本树没有 `sys_sleep` 是根因"，那是错的。
   pinned 测试仓的 `user_lib::sleep()` 是 `get_time() + yield_()` 忙等循环，**不发 syscall 101**
   （只有 `sleep_blocking()` 才发）；旧日志里 `Unsupported syscall_id` 命中数为 **0**。
   据此我曾移植整套内核 sleep（`Sleeping` 状态 + 唤醒 + 调度器兜底），核实后**已全部撤回**：
   它对判分不是必需的，且那条"全在睡就自旋等 mtime"的兜底路径在任何用例里都不可达。
2. **`#![deny(warnings)]`**：`os/src/main.rs` 使任何警告成为致命错误，未使用 import 会直接让
   `make kernel` 失败（本项目里 ch3 曾因 `sip` 未使用与缺 `pub use context::TrapContext` 从 5/7 掉到 0/7）。
   删调用方必须连带删被调方，否则 dead_code 同样致命。
3. **`sys_mmap`/`sys_munmap` 的 `len` 必须向上取整**，不能要求页对齐：
   `ch4_unmap` 断言 `mmap(start-len, len+1, 3) == 0`（那两页刚被 munmap 释放），
   而 `ch4_mmap3` 对同一形状的请求断言 `-1`（那两页仍被占用）——
   区分二者的唯一依据是"范围是否空闲"，不是 `len` 是否对齐。
   `ch4_unmap2` 的 `munmap(start, len+1)` 与 `munmap(start+1, len-1)` 都必须回 `-1`。
4. **判据要看 V 位，不是"页表项是否存在"**：`find_pte` 末级无条件返回 `Some`，
   映射一页会分配覆盖 2MB 的中间页表，同区其余页遂成为"存在但空"的 PTE。
   本树的 `is_range_free`/`is_range_mapped` 走 `page_table.is_mapped()`（已验 V 位）故无此问题，
   但第 5 章同名函数曾因此误拒合法映射——跨章移植时必须逐树核对，不能假设同构。

## 5. 状态

**16/16，`MAKE_RC=0`**，两轮独立复现（`oscamp/logs/c_ch4_sl3.log`、`c_ch4_stab.log`），
`rig_c_gaps.py ch4` 给出 `需补齐=0`、`不该出现却出现了=0`。
本报告与 `reports/lab1.md` 一起满足 `LAB := 2` 的报告闸门（两者均已 git 跟踪）。
判分器、`os/build.rs`、`os/Makefile`、`easy-fs-fuse/src/main.rs` 会被判分流程覆盖，本实验未改动它们。
CI 侧硬断言 QEMU 8.2.x，本地测量用 Homebrew QEMU 11.1.3，这是与 CI 的唯一实质环境差。

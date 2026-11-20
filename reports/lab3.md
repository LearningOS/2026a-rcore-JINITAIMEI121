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

## 3. 实测证据与根因（最终）

推进轨迹：`c_ch5_scan.log` 2/15 → `c_ch5_hand2.log` 7/15（清掉 7 个编译错误）→ 12/15 → **15/15**。
最终两轮独立测量均为满分：`oscamp/logs/c_ch5_mm2.log` 与 `c_ch5_stab.log`，
都是 `Test passed…: 15/15`、`MAKE_RC=0`、逐条判决行数 15（与分母一致）、
`rig_c_gaps.py ch5` 给出 `需补齐=0`、`不该出现却出现了=0`。

三个真实根因（按修复顺序）：

1. **`stride must be positive` 内核 panic**（`os/src/task/task.rs`）：优先级到 stride 的换算
   写出了非正数。它一次终止整个 usertest 批次，所以代价不是"一项失败"而是下游连锁缺失——
   `Test wait/waitpid/set_priority/spawn0` 与 sleep 家族都是被它拖掉的。
   **更正一条早期归因**：sleep 家族的缺失并非 `sys_sleep` 语义有问题。
   pinned 测试仓的 `user_lib::sleep()` 是 `get_time() + yield_()` 的忙等循环，**根本不发 syscall 101**
   （只有 `sleep_blocking()` 才发），所以这些用例考的是时钟与调度能否推进，而不是内核 sleep 原语。
2. **`sys_mmap`/`sys_munmap` 与冻结用例的契约相反**：原实现要求 `len` 页对齐、非法请求一律回 `0`。
   而 `ch4_mmap3` 断言未对齐/`prot=0`/高位 prot/重叠请求回 `-1`，`ch4_unmap` 又断言
   `mmap(start-len, len+1, 3)` 回 `0`。改为 **`len` 向上取整**、非法回 `-1`；
   `prot=2`（W 无 R）回 `0` 但**不建映射**——因为 `04_2/04_3` 的失败串在 `NOT_EXPECTED` 里，
   要求那次访问必须缺页，而 RISC-V 没有可编码的 write-only 叶子 PTE。
3. **"页表项存在 ≠ 页已映射"**（真正的拦路石）：`find_pte` 走到最后一级**无条件返回 `Some`**，
   而映射一页会分配覆盖整个 2MB 区域的中间页表，于是同区其余页变成"存在但空"的 PTE。
   `is_range_free`/`is_range_mapped`/`copied_byte_buffer`/`check_user_pages` 原先用
   `is_some()`/`translate()?` 判映射，导致 `mmap(start+len, len*2)` 在 `mmap(start,len)` 成功后被误拒；
   更危险的是空 PTE 的 `ppn()` 为 **0**，按原写法会把**内核物理页 0** 当用户内存读写。
   四处统一改验 **V 位**。随之失效的 `range_perm`/`set_area_perm_in_range`/`MapArea::set_perm`/
   `find_existing_pte` 一并删除（`deny(warnings)` 下 dead_code 致命）。

## 4. 坑

1. `#![deny(warnings)]`（`os/src/main.rs`）：任何警告即编译失败；删调用方必须连带删被调方。
2. 判分器覆盖 `os/build.rs`、`os/Makefile`、`easy-fs-fuse/src/main.rs`，改这些无效。
3. 内核 panic 会终止整个 usertest 批次，代价是下游多项连锁失败——修它优先于逐项目测。
4. 官方 QEMU 预算是 `timeout --foreground 30s`，**不可放宽**；任何用例挂住都会截断整批，
   使 MISS 列表在不同轮次之间漂移，看起来像"随机失败"。
5. CI 侧 `build.yml` 硬断言 **QEMU 8.2.x**，本地测量用的是 Homebrew QEMU 11.1.3；
   这是与 CI 的唯一实质环境差，报告结果时须带上这条口径差（新版 QEMU 对 RISC-V 规范更严格，
   且实测 qemu+判分约 9s，对 30s 预算有 3 倍余量）。

## 5. 状态

**15/15，`MAKE_RC=0`**，两轮独立复现。本报告与 `reports/lab1.md`、`reports/lab2.md`
一起满足 `LAB := 3` 的报告闸门（三者均已 git 跟踪）。

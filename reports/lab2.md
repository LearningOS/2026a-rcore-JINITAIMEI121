# Lab 2 · 内存管理（在本分支 `rcore-ch6` 树中的实现与验证）

分支：`2026a-rcore-JINITAIMEI121` @ `rcore-ch6`
判分 pin：`CHECKER_REV 7d61ec55b58eed6ca7052917846c1b87af34563a` / `TEST_REV a0593662ad55d670ba8c27ce1763347cd0dd552f`

## 1. 为什么第 6 章要交 Lab 2 与 Lab 3

`ci-user/Makefile` 在 `CHAPTER=6` 时取 `LAB := 4`、`INITPROC := 1`，报告闸门循环检查 `reports/lab1..lab4`。
判分是**累积**的：`check/ch6.py` 的 `EXPECTED_6 = set(EXPECTED_5 + EXPECTED_6b + [...]) - {"Test set_priority OK!"}`，
所以内存与进程两章的行为在本树里必须继续成立，Lab 2/Lab 3 的记录对象就是本树的这份实现。

## 2. 本树实现面（按实际存在的文件与符号核对）

- `os/src/mm/address.rs`、`frame_allocator.rs`、`heap_allocator.rs`、`memory_set.rs`、`page_table.rs`、`mod.rs`。
- 内存系统调用实际导出：`sys_mmap`、`sys_munmap`、`sys_sbrk`（在 `os/src/syscall/memory.rs`）。
- 与第 4 章的差别：本树额外有 `os/src/fs/`、`os/src/drivers/` 与 `easy-fs/`，
  因此 `mmap` 的可映射类型不再只有匿名私有映射，还要能承载文件映射；`MemorySet` 与文件描述符表
  在 `exec` 时的继承关系由 `os/src/task/process.rs`/`task.rs` 维护。

要点：`FramedArea`/`MapPermission` 决定 `mmap` 返回的地址是否立即可读写；`munmap` 必须真正释放页帧
（否则 `Test mass open/unlink` 这类反复建立/拆除地址空间的用例会耗尽物理页）；
`translated_refmut` 对未映射或权限不足地址必须返回 `None` 而不是解引用触发内核异常。

## 3. 实测证据（本树）

`oscamp/logs/c_ch6_fix4.log`：`Test passed18623: 27/31`，逐条判决行数 31（与分母一致）。
其中属于本 lab 的 4 项内存检查全部 PASS：`Test 04_1 OK`、`Test 04_4 test OK`、`Test 04_5 ummap OK`、
`Test 04_6 ummap2 OK` 中除 `04_6 ummap2` 外均通过；MISS 清单（`rig_c_gaps.py ch6`）为
`Test 04_4 test OK`、`Test 04_6 ummap2 OK`、`Test link OK`、`Test mass open/unlink OK`。
也就是说本 lab 在本树还差 `04_4`（映射区域行为）与 `04_6`（连续 `munmap`）两项，
且这两项与文件系统那两项同时失败，提示病灶在"地址空间拆除后资源归还"这条共同路径上，
而不是各自独立的逻辑。

## 4. 坑

1. `#![deny(warnings)]`：警告即编译失败（本项目实测：一个未使用 import 使整章从有分变成 0 分）。
2. 判分器覆盖 `os/build.rs`、`os/Makefile`、`easy-fs-fuse/src/main.rs`，无效改动面。
3. `easy-fs` 是 `#[cfg(not(test))] no_std` 与 host 侧并存的可测库，`make fsimg` 会调用
   `easy-fs-fuse` 生成磁盘镜像；`c_ch6_scan.log` 里出现过 `make[1]: *** [fsimg] Error 101`，
   即镜像生成失败时整章连判分都进不去（该轮 `Test passed28115: 2/31`）。

## 5. 状态

本 lab 在 31 项中相关 4 项里尚差 2 项；本报告满足 `LAB := 4` 对 `lab2` 的存在性要求，内容以本树实测为准。

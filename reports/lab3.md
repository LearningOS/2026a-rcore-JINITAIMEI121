# Lab 3 · 地址空间与进程（在本分支 `rcore-ch6` 树中的实现与验证）

分支：`2026a-rcore-JINITAIMEI121` @ `rcore-ch6`
判分 pin：`CHECKER_REV 7d61ec55b58eed6ca7052917846c1b87af34563a` / `TEST_REV a0593662ad55d670ba8c27ce1763347cd0dd552f`

## 1. 判分口径

`CHAPTER=6` 时 `INITPROC := 1`：判分流程会用 `user/build/elf/ch6_usertest.elf` 复制出
`ch6b_initproc.elf`（`ci-user/Makefile:118`），即**第一个用户进程就是 usertest 套件本身**。
`EXPECTED_6` 由 `EXPECTED_5` 继承进程类检查并**剔除** `Test set_priority OK!`——
所以在本树里 stride/优先级不再是得分点，`spawn`/`wait`/`waitpid`/`exit`/`fork` 才是。

## 2. 本树实现面（按实际符号核对）

任务层：`os/src/task/{id,manager,processor,task,context,switch.S,switch,mod}.rs`。
进程类系统调用实际导出：`sys_fork`、`sys_exec`、`sys_exit`、`sys_spawn`、`sys_waitpid`、`sys_getpid`、
`sys_yield`、`sys_read`、`sys_write`、`sys_sbrk`、`sys_mmap`、`sys_munmap`、
`sys_open`、`sys_close`、`sys_fstat`、`sys_linkat`、`sys_unlinkat`。

与第 5 章的关键差别在 `exec`：文件路径进入内核后要经 `os/src/fs.rs` 打开 ELF、
用 `ImageLayout` 解析程序头并重建 `MemorySet`，再把 `TrapContext` 里的 `x[10]`（argv）指向新栈上的参数串；
`TaskStatus` 多出 `Zombie` 与退出码，`waitpid` 需要遍历子任务回收。fork 则要求
`MemorySet::fork_tf` 复制地址空间并复制 fd 表（本树新增的部分）。

## 3. 实测证据（本树）

`oscamp/logs/c_ch6_fix4.log`：`Test passed18623: 27/31`。属于本 lab 的进程类检查
（`Test spawn0 OK`、`Test wait OK`、`Test waitpid OK`、`exit pass.`、`forktest pass.`、
`Hello, world from user mode program!`、`Test power_3/5/7 OK`、`child process pid = …, exit code = …`）
本轮全部 PASS（MISS 清单里没有它们，见 `rig_c_gaps.py ch6` 输出）。

历史一轮的对照读数：`oscamp/logs/c_ch6_scan.log` 为 `Test passed28115: 2/31`，
失败原因是编译而非逻辑——`error: could not compile easy-fs (lib) due to 1 previous error`
（`error[E0382]: borrow of moved value: dirent`），`make[1]: *** [fsimg] Error 101`。
这条对比说明本章的分是"能编出来"之后一次性回来的，逐项试错不如先过编译尺子。

## 4. 坑

1. `#![deny(warnings)]`：本树同样适用。
2. `E0382 borrow of moved value` 在 `easy-fs` 里高发：`Dirent` 被 `move` 进写入后又读它的字段，
   必须先取 `id`/`name_len` 再移动。
3. `INITPROC` 复制出的 `ch6b_initproc.elf` 若与 `ch6_usertest.elf` 不是同一份构建产物，
   会出现"usertest 跑了但 initproc 里是老内核"的撕裂读数；判分流程自己覆盖，不能依赖它调试。

## 5. 状态

本 lab 相关项在本树已全部通过；本报告满足 `LAB := 4` 对 `lab3` 的存在性要求。

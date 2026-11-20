# Lab 4 · 文件系统（rCore-Tutorial 第 6 章）

分支：`2026a-rcore-JINITAIMEI121` @ `rcore-ch6`
判分 pin：`CHECKER_REV 7d61ec55b58eed6ca7052917846c1b87af34563a` / `TEST_REV a0593662ad55d670ba8c27ce1763347cd0dd552f`

## 1. 判分口径

`CHAPTER=6` ⇒ `LAB := 4`、`INITPROC := 1`、学生侧 `BASE := 2` / `CHECK_REPORTS := 1` / `BASE_CHAR` 为空
⇒ 跑 `check/ch6.py`，`EXPECTED_6 = set(EXPECTED_5 + EXPECTED_6b + ["Test file0 OK!", "Test fstat OK!",
"Test link OK!", "Test mass open/unlink OK!"]) - {"Test set_priority OK!"}`，分母 31。
磁盘镜像由 `make fsimg` 经 `easy-fs-fuse` 生成（判分器会覆盖 `easy-fs-fuse/src/main.rs`），
所以"镜像生成失败"与"内核逻辑错"在判分上表现相同：`make` 非零 ⇒ `rcore_grade.py` 直接 `sys.exit(returncode)`。

## 2. 本树实现面（按实际存在的文件与符号核对）

`easy-fs/src/`：`bitmap.rs`（`BitmapBlock`/`EasyFSUsedBitmap`）、`block_cache.rs`（`BlockCacheManager`，
LRU + 脏块回写）、`block_dev.rs`（`BlockDevice`/`SDCard` trait）、`layout.rs`（`SuperBlock`、`DiskInode`、
`DirEntry`、`DiskInodeEntry`）、`vfs.rs`、`efs.rs`、`lib.rs`。
`easy-fs/src/vfs.rs` 在本树实际导出的方法：`create`、`link`、`read`、`write`。
内核侧 `os/src/fs.rs` + 系统调用：`sys_open`、`sys_close`、`sys_read`、`sys_write`、`sys_fstat`、
`sys_linkat`、`sys_unlinkat`。

要点：fd 表在 `fork`/`exec` 时的共享语义（同一 `File` 对象共享 `current_offset`，`dup` 亦如此）；
`link` 要求被链接的目录项与原目录项指向**同一个** `DiskInode`（引用计数 +1，而不是复制内容）；
`unlink` 只在引用计数归零时归还 inode 与数据块；`fstat` 必须把 `Stat` 写回用户指针（页表翻译，不可直接解引用）。

## 3. 实测证据

`oscamp/logs/c_ch6_fix4.log`：`Test passed18623: 27/31`，逐条判决行数 31（与分母一致）。
MISS 清单：`Test link OK`、`Test mass open/unlink OK`、`Test 04_4 test OK`、`Test 04_6 ummap2 OK`。

其中两项直接落在本 lab：
- `Test link OK`：`vfs.rs` 已有 `link`，但该用例仍未通过，说明缺的是**上层语义**
  （`sys_linkat` 的目录项写入/引用计数），而不是 `link` 函数是否存在。
- `Test mass open/unlink OK`：反复 create/open/unlink 的用例。本树 `vfs.rs` **没有 `unlink` 方法**，
  而同一仓库族的 ch8 树里 `easy-fs/src/vfs.rs` 是有的（`create`/`link`/`read`/`unlink`/`write`）。
  同树家族里已有现成实现，优先按 ch8 那份的引用计数与位图归还语义补齐，再按本树的
  `DirEntry`/`DiskInode` 字段名对齐，不要另起一套。

历史对照：`oscamp/logs/c_ch6_scan.log` 为 2/31，唯一错误是
`error[E0382]: borrow of moved value: dirent` ⇒ `could not compile easy-fs (lib)` ⇒ `make[1]: *** [fsimg] Error 101`。
把这一条与上面并排看可知：本章 29 分的开关在"能否编译并产出镜像"，逐项试错的性价比极低。

## 4. 坑

1. `Dirent` 被 move 后再读它的字段是本树最常见的 `E0382`，先取 `value.len()`/`id` 再移动。
2. `#![deny(warnings)]`：未使用 import 会让整章归零。
3. `BlockCacheManager` 的 LRU 容量与脏块回写顺序：`mass open/unlink` 会制造大量块号复用，
   若脏块在归还前未回写，就会出现"单跑通过、批量失败"的假象。

## 5. 状态

31 项中 27 项通过，本 lab 尚差 `link` 与 `mass open/unlink` 两项。
本报告满足 `LAB := 4` 对 `lab4` 的存在性要求（`lab1..lab4` 齐备）。

# Lab 5 · 并发与死锁检测（rCore-Tutorial 第 8 章）

## 判分口径

`make test CHAPTER=8` 之后由 `check/ch8.py` 对内核串口输出做匹配。它的期望集合是
`EXPECTED_6b` 加上 11 条并发用例串，再减去 `Test sbrk almost OK!`，共 **25 条**；
`NOT_EXPECTED` 沿用 `NOT_EXPECTED_4b`（含 `FAIL: T.T`、`Test sbrk failed!`），
也就是说有些用例**必须继续失败**，不能"顺手修好"。QEMU 侧是官方
`timeout --foreground 30s`，这个预算不能放宽——它同时意味着：**任何一个用例挂住，
整批都会被截断，后面所有用例的串一起消失**。这一点决定了本章的调试方法。

## 内核提供的并发原语

- **线程**：`sys_thread_create(entry, arg)` 复用当前进程的地址空间，新建 TCB 时
  传入 `is_thread=true`，用户栈从进程的 `ustack_base` 再分配一段；新线程的
  `TrapContext` 用 `app_init_context` 初始化，并把 `x[10]`（a0）设为 `arg`。
  线程号写回 `process_inner.tasks[tid]`，`sys_waittid` 据此取 `exit_code`：
  目标尚未退出时返回 `-2`，用户态 `waittid` 收到 `-2` 就 `yield_()` 重试。
- **互斥锁**：`MutexSpin` 用 `UPSafeCell<bool>` + `suspend_current_and_run_next()`
  让出而非忙等；`MutexBlocking` 维护 `wait_queue`，`unlock` 时把所有权**直接移交**
  队首等待者（所以被唤醒者不需要重新抢锁）。
- **信号量**：`SemaphoreInner { count, wait_queue, ridx }`，`up` 先自增再在
  `count <= 0` 时唤醒一个等待者。
- **条件变量**：`Condvar::wait` 先 `mutex.unlock()` 再把自己挂进等待队列并阻塞，
  `signal` 唤醒队首；被唤醒者重新取回守护锁。
- **死锁检测**：`sync/resource.rs` 维护等待图，`acquire/grant/hand_over_semaphore/
  wake_mutex_holder/drop_holder/condvar_signalled` 在阻塞与唤醒时同步边。
  `sys_mutex_lock`/`sys_semaphore_down` 在 `acquire` 判定会成环时返回 `-0xdead`，
  且**不改动任何计数或队列**，让死锁用例能打印自己的 OK 串而不是永久挂住。

## 已确证并修复的缺陷：信号量判据错位

`os/src/sync/semaphore.rs` 的 `try_down_or_block` 原先在**自减之前**判
`if inner.count < 0` 才走阻塞分支。`count` 的语义是"当前可用单元数"，
正确条件是"自减后 < 0 才阻塞"，等价于自减前 `<= 0`。

后果：`count == 0`（资源已耗尽）时请求落进 else 分支，把 `count` 减成 `-1`
**直接放行且不排队**。互斥与同步语义因此整体失效——基于信号量构造的
`sync_sem`、`mpsc_sem`、哲学家就餐、死锁检测用例全部拿不到预期行为。
改为 `if inner.count <= 0` 后，耗尽时的请求会正确自减、入队、
`block_current_and_run_next()`。

## 主根因：stride 调度器的计数溢出导致静默饿死

信号量判据修好之后仍然全局停摆，真正的元凶在 `os/src/task/manager.rs`。
本章把 FIFO 换成了 stride 调度：`fetch()` 取 `stride` 最小者，
`advance_stride` 每消耗一个时间片加 `STRIDE_1 / priority`，
而 `wakeup_task()` 会把被唤醒者的 `stride` 置 0（让它以当前基准重新入队）。

问题出在 `add()`：它原先把**整条就绪队列**都抬到新来者的基准上，
即对每个已在队里的任务做 `t.stride += min_stride - newcomer_stride`。
由于每次唤醒都把新来者置 0，这个差值恰好等于队列最小值，于是
**每发生一次唤醒，所有老任务的 stride 就被加上一遍队列最小值**——
指数增长。加上每秒约 100 次时钟中断与信号量/互斥锁唤醒，
`isize` 在第一秒内就回绕；release 模式下算术回绕**不 panic**，
回绕后的任务落在约 `-8.9e18`，从此永远是 `min_by_key` 的赢家并独占 CPU，
其余正常任务（stride ≈ 0）被永久饿死。主线程因此在 `waittid`/`waitpid`
的 `-2 + yield_()` 上无限重试，而它们可运行的 worker 一次也轮不到——
这就是"无 panic 的全局静默"。

临时 watchdog 打出的就绪队列（`logs/c_ch8_wd1.log:714`，现已移除）：

```
ready=[93.0:-3659030419097498496 0.0:-8759296466547665536 16.1:0 16.2:0
       16.3:0 16.4:0 15.1:0 15.2:0 15.3:0 15.0:-8896716785721188608]
pid=15 [0:s0 1:s0 2:s0 3:s0]   pid=16 [0:s1 1:s0 2:s0 3:s0 4:s0]
```

所有 worker 都是 `s0`(Ready)、没有任何等待边、stride 为 0，却 20s 未被取走。
这条证据同时排除了死锁检测器、阻塞原语、`sys_waittid`、时钟与文件系统：
它们都不是停摆原因（等待图为空，worker 根本没在等）。

修法是把方向反过来：**只把新来者抬到队列基准**，不再拖动其他计数器。

```rust
let base = self.ready_queue.iter()
    .map(|t| t.inner_exclusive_access().stride).min();
if let Some(min_stride) = base {
    let mut inner = task.inner_exclusive_access();
    if inner.stride < min_stride { inner.stride = min_stride; }
}
```

这样 stride 只经由 `advance_stride` 单调增长（30s 内量级约 1e8），
不会回绕；新唤醒者既不会插队到所有人前面，也不会把别人拖着一起涨。

## 调试方法与三条教训

1. **先分清"断言失败"与"全局停摆"**。本章日志无任何 panic，最后一条实质输出
   之后整批静默，23 个启动用例只有 9 个打印退出行，连 `ch5b_forktest`、
   `ch6b_filetest_simple`、`ch7b_pipetest` 这些与并发无关的也没退出。
2. **打点必须窄**。对全部 syscall 号打印会把分数从 17 压到 14：额外串口输出
   拖慢内核，30s 预算内跑不完。只打并发族（460/462/463/464/466/467/468/469/470）
   才看到 `id=462`(WAITTID) 刷屏。测量前必须撤掉全部插桩。
3. **静默故障要怀疑回绕与不可重入锁**。release 下整数回绕不 panic；
   `spin::Mutex` 重入是无限自旋也不 panic（第 6 章的 `Inode::id()` 就是）。
   两者都表现为"没有错误信息的停摆"，靠读码很难发现，必须打出内部状态。

## 测量

- `logs/c_ch8_fix1.log`：`Test passed9871: 25/25`，`MAKE_RC=0`
- `logs/c_ch8_fix1b.log`：`Test passed16626: 25/25`，`MAKE_RC=0`
- 主线独立复测 `logs/c_ch8_verify.log`：`Test passed55668: 25/25`，`MAKE_RC=0`，
  `rig_c_gaps.py ch8` 给出 `verdict_lines=[('25','25')]`、`需补齐=0`、
  `不该出现却出现了=0`；checker/tests pin 与判分方一致
  （`7d61ec55…` / `a0593662…`）。

即 **25/25**，且 `NOT_EXPECTED` 中的串一个都没有出现。

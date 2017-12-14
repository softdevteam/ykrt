// Copyright (c) 2017 King's College London
// created by the Software Development Team <http://soft-dev.org/>
//
// The Universal Permissive License (UPL), Version 1.0
//
// Subject to the condition set forth below, permission is hereby granted to any
// person obtaining a copy of this software, associated documentation and/or
// data (collectively the "Software"), free of charge and under any and all
// copyright rights in the Software, and any and all patent rights owned or
// freely licensable by each licensor hereunder covering either (i) the
// unmodified Software as contributed to or provided by such licensor, or (ii)
// the Larger Works (as defined below), to deal in both
//
// (a) the Software, and
// (b) any piece of software and/or hardware listed in the lrgrwrks.txt file
// if one is included with the Software (each a "Larger Work" to which the Software
// is contributed by such licensors),
//
// without restriction, including without limitation the rights to copy, create
// derivative works of, display, perform, and distribute the Software and make,
// use, sell, offer for sale, import, export, have made, and have sold the
// Software and the Larger Work(s), and to sublicense the foregoing rights on
// either these or other terms.
//
// This license is subject to the following condition: The above copyright
// notice and either this complete permission notice or at a minimum a reference
// to the UPL must be included in all copies or substantial portions of the
// Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate env_logger;

use std::collections::HashMap;
use std::sync::RwLock;
use std::thread;
use std::thread::JoinHandle;
use std::cell::RefCell;


/// Uniquely identifies a location in the user-program.
///
/// XXX Hard-coded to a simple program counter value.
/// XXX This need to be user-specified, perhaps using a trait?
type JitLocation = u32;

/// The type used to count how many times a JitLocation has been interpreted.
type HotCount = u32;

/// The number of times a location is interpreted before we trace it.
/// XXX make this configurable.
const HOT_THRESHOLD: HotCount = 10;

/// The shared trace store.
///
/// Maps a location to a trace (if one exists). Locations not in the map have not yet been traced.
lazy_static! {
    static ref TRACES: RwLock<HashMap<JitLocation, JitTrace>> = RwLock::new(HashMap::new());
}

/// Execute the trace for the specified location (if there is one).
///
/// Returns true if a trace was present (and thus was executed), or false otherwise.
fn try_exec_trace(loc: JitLocation) -> bool {
    let trace_map = TRACES.read().unwrap();
    let trace = trace_map.get(&loc);
    if trace == Some(&JitTrace::FinalisedTrace) {
        JitThreadContext::phase_transition(JitPhase::RunningTrace(loc));
        // XXX Really execute the trace.
        JitThreadContext::phase_transition(JitPhase::Interpreting);
        return true;
    }
    false
}

/// Is a thread currently tracing/compiling the specified location?
fn trace_pending(loc: JitLocation) -> bool {
    let trace_map = TRACES.read().unwrap();
    match trace_map.get(&loc) {
        Some(&JitTrace::TracePending) => return true,
        _ => return false,
    };
}

/// Store a trace (or "trace pending" marker in the trace cache for the specified location.
fn set_trace(loc: JitLocation, trace: JitTrace) {
    // Sanity checks.
    {
        // Extra scope for dropping the lock prior to the write() lock later (avoids deadlock).
        let trace_map = TRACES.read().unwrap();
        let old_val = trace_map.get(&loc);
        if trace == JitTrace::TracePending {
            // If we are collecting a trace, then the location should not already be in the trace cache.
            assert!(old_val == None);
        } else if trace == JitTrace::FinalisedTrace {
            // If we are storing a finished trace, then it should have been marked as "pending" first.
            assert!(old_val == Some(&JitTrace::TracePending));
        }
    }

    TRACES.write().unwrap().insert(loc, trace);
}

/// Shared hot counts.
///
/// Maps a location to its hot count. Increments of the counter are atomic due to the RwLock.
lazy_static! {
    static ref HOT_COUNTS: RwLock<HashMap<JitLocation, HotCount>> = RwLock::new(HashMap::new());
}

/// Get the hot count for the given location.
fn hot_count(loc: JitLocation) -> HotCount {
    let ret = match HOT_COUNTS.read().unwrap().get(&loc) {
        Some(v) => *v,
        None => 0,
    };
    ret
}

/// Atomically increment and return the hot count for the given location.
fn incr_hot_count(loc: JitLocation) -> HotCount {
    // XXX don't store locations which cannot be loop heads.
    let mut counts = HOT_COUNTS.write().unwrap();
    let loc_count = counts.entry(loc).or_insert(0);
    *loc_count += 1;
    *loc_count
}

/// Thread-specific meta-tracer context.
///
/// Each thread has one of instance of this inside a thread-local.
#[derive(Debug)]
pub struct JitThreadContext {
    phase: JitPhase,
    // XXX PT packet buffer goes here.
}

impl JitThreadContext {
    pub fn new() -> Self {
        Self {
            phase: JitPhase::Interpreting,
        }
    }

    /// Resets the per-thread JIT state to defaults.
    #[cfg(test)]
    fn reset(&mut self) {
        self.phase = JitPhase::Interpreting;
    }

    /// Get the current thread's JIT phase.
    fn current_phase() -> JitPhase {
        let phase = JIT_THREAD_CX.with(|k| {
            k.borrow().phase.clone()
        });
        phase
    }

    /// Transition to a new JIT phase.
    fn phase_transition(phase: JitPhase) {
        JIT_THREAD_CX.with(|k| {
            k.borrow_mut().phase = phase;
        });
    }

    /// Compile a trace from the raw PT packets (in parallel with the interpreter)..
    ///
    // XXX eventually we probably want a thread pool, as having an unbounded number of threads
    // is probably bad.
    fn compile_trace(loc: JitLocation) {
        let name = format!("compile_loc_{}", loc);
        debug!("new compilation thread: {}", name);
        // By discarding the resultant JoinHandle here, we are creating "detached threads".
        named_thread(name, move || {
                // XXX This is where we would invoke the real trace compiler. For now we just
                // pretend to, and insert a dummy trace.
                set_trace(loc, JitTrace::FinalisedTrace);
                debug!("compilation thread {} terminating", debug_thread_name());
        });
    }
}

thread_local! {
    static JIT_THREAD_CX: RefCell<JitThreadContext> = RefCell::new(JitThreadContext::new());
}

/// Indicates what a thread is doing at any given time.
#[derive(Debug, Eq, PartialEq, Clone)]
enum JitPhase {
    Interpreting,               // Interpreting and profiling.
    Tracing(JitLocation),       // Recording a trace from the specified location.
    RunningTrace(JitLocation),  // Running a trace starting from the specified location.
}

/// A machine code trace for a location.
#[derive(Debug, PartialEq, Eq)]
enum JitTrace {
    TracePending,       // Currently being traced or compiled.
    FinalisedTrace,     // A finished trace with executable code.
}

fn debug_thread_name<'a>() -> String {
    let thr = std::thread::current();
    let sref = match thr.name() {
        Some(n) => n,
        None => "no_name",
    };
    String::from(sref)
}

/// JIT Control Point.
///
/// The interpreter will need to call this periodically to inform the meta-tracer where in the
/// user-program we are. For example, in a bytecode interpreter, the JIT control point will be
/// called immediately inside the bytecode dispatch loop.
///
/// This function is passed a JitLocation, which should uniquely identify the position in the
/// user-program (usually this includes at least a program counter value). The JIT will use this
/// information for profiling, trace collection, and trace execution.
///
/// The control point returns a JitTransition indicating the work the JIT undertook.
pub fn jit_control_point(loc: JitLocation) -> JitTransition {
    debug!("JIT Control: thread={:?}: loc={:?}, hot_count={}",
           debug_thread_name(), loc, hot_count(loc));

    let ret;
    match JitThreadContext::current_phase() {
        JitPhase::Interpreting => {
            let ran = try_exec_trace(loc);
            if ran {
                ret = JitTransition::ExecutedTrace;
            } else {
                // There is no trace for this location (yet), so we just increment the hot count.
                let ct = incr_hot_count(loc);
                if ct == HOT_THRESHOLD && !trace_pending(loc) {
                    // Location becomes hot.
                    set_trace(loc, JitTrace::TracePending);
                    // XXX start Intel PT.
                    JitThreadContext::phase_transition(JitPhase::Tracing(loc));
                    ret = JitTransition::StartedTracing;
                } else {
                    ret = JitTransition::IncrementedHotCount;
                }
            }
        },
        JitPhase::Tracing(start_loc) => {
            // If we have traced one iteration of the loop, stop tracing.
            if loc == start_loc {
                // XXX stop Intel PT.
                JitThreadContext::compile_trace(loc);
                JitThreadContext::phase_transition(JitPhase::Interpreting);
                ret = JitTransition::FinishedTracing;
            } else {
                // Still tracing.
                ret = JitTransition::ContinuedTracing;
            }
        },
        JitPhase::RunningTrace(_) => {
            // The JIT control point should not be called from within a trace.
            unreachable!();
        },
    }
    debug!("JIT Control: thread={:?}: loc={:?}, hot_count={}, trans={:?}",
           debug_thread_name(), loc, hot_count(loc), ret);
    ret
}

/// JIT Transitions.
///
/// The JIT control point uses this to communicate what it just did.
#[derive(Debug, Eq, PartialEq)]
pub enum JitTransition {
    IncrementedHotCount,
    StartedTracing,
    ContinuedTracing,
    // AbortedTracing XXX
    FinishedTracing,
    ExecutedTrace,
}

/// A helper to spawn a named thread.
///
/// The thread executes the `f` closure, under a thread named `name`.
fn named_thread<'s, F, N>(name: N, f: F) -> JoinHandle<()>
               where F: FnOnce(), F: Send + 'static, N: Into<String> {
    let thr = thread::Builder::new()
                              .name(name.into())
                              .spawn(f)
                              .unwrap();
    thr
}

#[cfg(test)]
mod tests {
    use ::*;
    use std::time;

    /// The number of times to repeat multi-threaded tests.
    const MULTITHREAD_REPS: u8 = 10;

    /// Reset the JIT state.
    ///
    /// This is needed to run successive tests within one process, because we use global state and
    /// thread-locals. To truly reset the JIT, all tests must also ensure that any threads it
    /// creates are also joined.
    fn reset_jit() {
        // Reset shared JIT state.
        TRACES.write().unwrap().clear();
        HOT_COUNTS.write().unwrap().clear();

        // Reset per-thread JIT state. Assuming a single thread for now.
        JIT_THREAD_CX.with(|k| {
            k.borrow_mut().reset();
        });
    }

    /// Test helper to artificially force a trace to be collected for the given location.
    ///
    /// The hot counts and JIT transitions are checked after each call to the JIT control point.
    ///
    /// The hot count for the desired location must be zero before calling this and you must be
    /// very careful when using this function in multi-threaded tests. Two threads must not
    /// concurrently call this function on the same location, otherwise the hot counts and JIT
    /// transitions cannot be checked due to non-determinism.
    fn dummy_trace_location(loc: JitLocation) {
        // Interpreting phase.
        for i in 0..(HOT_THRESHOLD - 1) {
            assert!(hot_count(loc) == i);
            assert!(JitThreadContext::current_phase() == JitPhase::Interpreting);
            assert!(jit_control_point(loc) == JitTransition::IncrementedHotCount);
        }

        // The next iteration should trigger tracing.
        assert!(hot_count(loc) == HOT_THRESHOLD - 1);
        assert!(JitThreadContext::current_phase() == JitPhase::Interpreting);
        assert!(jit_control_point(loc) == JitTransition::StartedTracing);

        // When the start of the loop being traced shows up, we should fall back to the interpreter
        // while the trace is being compiled.
        assert!(JitThreadContext::current_phase() == JitPhase::Tracing(loc));
        assert!(jit_control_point(loc) == JitTransition::FinishedTracing);
        assert!(JitThreadContext::current_phase() == JitPhase::Interpreting);
    }

    // Continually call the JIT control point until a trace executes for the given location.
    fn dummy_exec_trace(loc: JitLocation) {
        let mut tries = 0;
        let sleep_time = time::Duration::from_millis(20);
        let max_tries = 1000;

        loop {
            match jit_control_point(loc) {
                JitTransition::ExecutedTrace => break,  // The trace appeared and ran, good.
                _ => {},                                // Still waiting.
            }
            tries += 1;
            if tries == max_tries {
                panic!("timed out waiting for trace to execute");
            }
            thread::sleep(sleep_time);
        }
    }

    /// Test the most basic scenario.
    #[test]
    fn test_jit_0() {
        reset_jit();
        dummy_trace_location(666);
        dummy_exec_trace(666);
    }

    /// Test tracing a few different locations.
    #[test]
    fn test_jit_lifecycle_1() {
        reset_jit();
        dummy_trace_location(666);
        dummy_trace_location(777);
        dummy_trace_location(888);
        dummy_exec_trace(666);
        dummy_exec_trace(777);
        dummy_exec_trace(888);
    }

    /// Test tracing a few different locations in a weird order.
    #[test]
    fn test_jit_lifecycle_2() {
        reset_jit();
        dummy_trace_location(888);
        dummy_trace_location(666);
        dummy_exec_trace(888);
        dummy_trace_location(777);
        dummy_exec_trace(777);
        dummy_exec_trace(666);
    }

    /// Guidelines for writing multi-threaded tests in this module.
    ///
    /// 1) Ensure you join all explicitly spawned threads before the test ends.
    ///
    /// 2) Ensure that all compilation threads have exited before the end of the test. The best way
    ///    to do this is to use dummy_exec_trace() to check that the trace you are expecting the
    ///    compilation thread to produce can be executed.
    ///
    /// 3) Use dummy_trace_location() carefully as it won't tolerate non-determinism caused by
    ///    threads. See the doc comment for this function for more info.

    /// Two threads, each tracing and executing a different location.
    #[test]
    fn test_jit_multithread_0() {
        for _ in 0..MULTITHREAD_REPS {
            reset_jit();

            let thr2 = named_thread("thr2", move || {
                dummy_trace_location(666);
                dummy_exec_trace(666);
            });

            dummy_trace_location(777);
            dummy_exec_trace(777);

            thr2.join().unwrap();
        }
    }

    /// Two threads, executing traces collected on the other thread.
    #[test]
    fn test_jit_multithread_1() {
        for _ in 0..MULTITHREAD_REPS {
            reset_jit();

            let thr2 = named_thread("thr2", move || {
                dummy_trace_location(666);
            });
            thr2.join().unwrap();

            dummy_exec_trace(666);
        }
    }

    /// Two threads, collecting/executing various traces.
    #[test]
    fn test_jit_multithread_2() {
        for _ in 0..MULTITHREAD_REPS {
            reset_jit();

            let thr2 = named_thread("thr2", move || {
                dummy_exec_trace(888);
                dummy_exec_trace(666);
                dummy_exec_trace(999);
            });

            dummy_exec_trace(666);
            dummy_exec_trace(777);
            dummy_exec_trace(888);

            thr2.join().unwrap();
        }
    }

    // Three threads all operating on the same location. Check the trace isn't collected three
    // times (this would fire an assertion in `JitThreadContext::compile_trace()`.
    #[test]
    fn test_jit_multithread_3() {
        for _ in 0..MULTITHREAD_REPS {
            reset_jit();

            let thr2 = named_thread("thr2", move || {
                dummy_exec_trace(666);
            });

            let thr3 = named_thread("thr3", move || {
                dummy_exec_trace(666);
            });

            dummy_exec_trace(666);

            thr2.join().unwrap();
            thr3.join().unwrap();
        }
    }
}

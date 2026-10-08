using System;
using System.Threading;
using UnityEditor;

namespace Unterm.Editor
{
    /// <summary>
    /// Background thread that computes Roslyn signature help (parameter hints) off the
    /// typing path, coalescing requests like <see cref="UntermCompletionWorker"/>: only
    /// the latest pending request is processed. The main thread submits cheaply and
    /// polls <see cref="TryTake"/> for the sequence it's waiting on.
    /// </summary>
    internal static partial class UntermSignatureWorker
    {
        private struct Request { public long Seq; public string Text; public int Pos; }

        private static readonly object s_inLock = new object();
        private static Request s_pending;
        private static bool s_hasPending;
        private static long s_seq;

        private static long s_resultSeq = -1;
        private static UntermRoslynCompletion.SigHelp s_result;

        private static AutoResetEvent s_signal;
        private static Thread s_thread;
        private static volatile bool s_stop;
        private static bool s_accepting;
        private static bool s_shutdownUnconfirmed;

        // Stop and restart explicitly; CoreCLR may retain these statics.
#if UNITY_7000_0_OR_NEWER
        [Unity.Scripting.LifecycleManagement.OnCodeInitializing]
#else
        [InitializeOnLoadMethod]
#endif
        private static void RegisterShutdown()
        {
#if !UNITY_7000_0_OR_NEWER
            AssemblyReloadEvents.beforeAssemblyReload -= Shutdown;
            AssemblyReloadEvents.beforeAssemblyReload += Shutdown;
#endif
            EditorApplication.quitting -= Shutdown;
            EditorApplication.quitting += Shutdown;
            lock (s_inLock) s_accepting = true;
        }

#if UNITY_7000_0_OR_NEWER
        [Unity.Scripting.LifecycleManagement.OnCodeUnloading]
#endif
        private static void Shutdown()
        {
#if !UNITY_7000_0_OR_NEWER
            AssemblyReloadEvents.beforeAssemblyReload -= Shutdown;
#endif
            EditorApplication.quitting -= Shutdown;
            Thread thread;
            lock (s_inLock)
            {
                s_accepting = false;
                s_stop = true;
                s_pending = default;
                s_hasPending = false;
                s_result = null;
                s_resultSeq = -1;
                thread = s_thread;
                if (thread != null && thread.IsAlive) s_signal?.Set();
            }
            if (thread != null && thread.IsAlive && !thread.Join(500))
            {
                lock (s_inLock) s_shutdownUnconfirmed = true;
                return;
            }
            lock (s_inLock)
            {
                if (s_thread == thread) { s_thread = null; s_signal = null; s_shutdownUnconfirmed = false; }
            }
        }

        public static long Submit(string text, int pos)
        {
            long seq;
            lock (s_inLock)
            {
                seq = ++s_seq;
                if (s_accepting && s_shutdownUnconfirmed && s_thread != null && s_thread.IsAlive)
                    UntermLog.WarnOnce("signature.shutdown", new TimeoutException(
                        "Worker stop is unconfirmed; new requests are unavailable until the owned thread exits."));
                if (!s_accepting || !EnsureThread())
                {
                    s_resultSeq = seq;
                    s_result = null;
                    return seq;
                }
                s_pending = new Request { Seq = seq, Text = text, Pos = pos };
                s_hasPending = true;
                s_signal.Set();
            }
            return seq;
        }

        public static bool TryTake(long seq, out UntermRoslynCompletion.SigHelp result)
        {
            lock (s_inLock)
            {
                if (s_resultSeq == seq)
                {
                    result = s_result;
                    s_result = null;
                    s_resultSeq = -1;
                    return true;
                }
            }
            result = null;
            return false;
        }

        // Called with s_inLock held. Never reset a still-running stopped generation.
        private static bool EnsureThread()
        {
            if (s_thread != null && s_thread.IsAlive) return !s_stop;
            s_signal = new AutoResetEvent(false);
            s_stop = false;
            s_thread = new Thread(Loop) { IsBackground = true, Name = "UntermSignature" };
            try { s_thread.Start(); }
            catch (Exception e)
            {
                s_signal.Dispose();
                s_signal = null;
                s_thread = null;
                s_stop = true;
                UntermLog.WarnOnce("signature.start", e);
                return false;
            }
            s_shutdownUnconfirmed = false;
            return true;
        }

        private static void Loop()
        {
            var signal = s_signal;
            try { ProcessRequests(signal); }
            finally
            {
                lock (s_inLock)
                {
                    s_stop = true;
                    if (ReferenceEquals(s_signal, signal)) s_signal = null;
                    signal.Dispose();
                }
            }
        }

        private static void ProcessRequests(AutoResetEvent signal)
        {
            while (true)
            {
                signal.WaitOne();
                if (s_stop) return;
                while (true)
                {
                    Request req;
                    lock (s_inLock)
                    {
                        if (s_stop) return;
                        if (!s_hasPending) break;
                        req = s_pending;
                        s_pending = default;
                        s_hasPending = false;
                    }
                    UntermRoslynCompletion.SigHelp r;
                    try { r = UntermRoslynCompletion.SignatureHelp(req.Text, req.Pos); }
                    catch (Exception e)
                    {
                        r = null;
                        lock (s_inLock)
                        {
                            if (!s_stop) UntermLog.WarnOnce("signature.worker", e);
                        }
                    }
                    lock (s_inLock)
                    {
                        if (s_stop) return;
                        s_resultSeq = req.Seq;
                        s_result = r;
                    }
                }
            }
        }
    }
}

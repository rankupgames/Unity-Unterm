using System;
using System.Collections.Generic;
using System.Threading;
using UnityEditor;

namespace Unterm.Editor
{
    /// <summary>
    /// A single long-lived background thread that runs Roslyn completion off the main
    /// thread (LSP-style), shared by all code-editor windows. Requests are COALESCED:
    /// only the latest pending request is processed, so rapid typing never piles up
    /// analyses. The main thread submits (cheap) and polls <see cref="TryTake"/> for the
    /// result of the sequence number it's waiting on.
    /// </summary>
    internal static partial class UntermCompletionWorker
    {
        // Mode: 0 = general (scope symbols), 1 = member (after `.`), 2 = attribute (after `[`).
        private struct Request { public long Seq; public string Text; public int Pos; public int Mode; }

        private static readonly object s_inLock = new object();
        private static Request s_pending;
        private static bool s_hasPending;
        private static long s_seq;

        private static long s_resultSeq = -1;
        private static List<(string insert, string label, char kind)> s_result;

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

        public static long Submit(string text, int pos, int mode)
        {
            long seq;
            lock (s_inLock)
            {
                seq = ++s_seq;
                if (s_accepting && s_shutdownUnconfirmed && s_thread != null && s_thread.IsAlive)
                    UntermLog.WarnOnce("completion.shutdown", new TimeoutException(
                        "Worker stop is unconfirmed; new requests are unavailable until the owned thread exits."));
                if (!s_accepting || !EnsureThread())
                {
                    s_resultSeq = seq;
                    s_result = null;
                    return seq;
                }
                s_pending = new Request { Seq = seq, Text = text, Pos = pos, Mode = mode };
                s_hasPending = true;
                s_signal.Set();
            }
            return seq;
        }

        /// If the result for exactly <paramref name="seq"/> is ready, hand it back and
        /// clear it. (Older results are overwritten by newer ones and never match.)
        public static bool TryTake(long seq, out List<(string insert, string label, char kind)> result)
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
            s_thread = new Thread(Loop) { IsBackground = true, Name = "UntermCompletion" };
            try { s_thread.Start(); }
            catch (Exception e)
            {
                s_signal.Dispose();
                s_signal = null;
                s_thread = null;
                s_stop = true;
                UntermLog.WarnOnce("completion.start", e);
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
                // Drain: always process the LATEST pending request; if newer ones
                // arrive while computing, the slot holds only the newest, so older
                // ones are coalesced away.
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
                    List<(string insert, string label, char kind)> r;
                    try
                    {
                        switch (req.Mode)
                        {
                            case 1: r = UntermRoslynCompletion.MemberCompletions(req.Text, req.Pos); break;
                            case 2: r = UntermRoslynCompletion.AttributeCompletions(req.Text, req.Pos); break;
                            case 3: r = UntermRoslynCompletion.TypeCompletions(req.Text, req.Pos); break;
                            case 4: r = UntermRoslynCompletion.NamespaceCompletions(req.Text, req.Pos); break;
                            case 5: r = UntermRoslynCompletion.OverrideCompletions(req.Text, req.Pos); break;
                            default: r = UntermRoslynCompletion.GeneralCompletions(req.Text, req.Pos); break;
                        }
                    }
                    catch (Exception e)
                    {
                        r = null;
                        lock (s_inLock)
                        {
                            if (!s_stop) UntermLog.WarnOnce("completion.worker", e);
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

using System;
using System.Diagnostics;
using System.IO;
using UnityEditor;
using UnityEngine;

namespace Unterm.Proof
{
    public static partial class ShutdownMarker
    {
#if UNITY_7000_0_OR_NEWER
        [Unity.Scripting.LifecycleManagement.OnCodeInitializing]
#else
        [InitializeOnLoadMethod]
#endif
        private static void Initialize()
        {
            string root = Environment.GetEnvironmentVariable("UNTERM_LIFECYCLE_PROOF");
            if (string.IsNullOrEmpty(root) || !File.Exists(Path.Combine(root, "owner.pid")) ||
                File.ReadAllText(Path.Combine(root, "owner.pid")) != Process.GetCurrentProcess().Id.ToString()) return;
            EditorApplication.quitting -= Quitting;
            EditorApplication.quitting += Quitting;
            File.WriteAllText(Path.Combine(root, "ready.json"), JsonUtility.ToJson(
                new Identity { pid = Process.GetCurrentProcess().Id, editorVersion = Application.unityVersion }));
        }
        private static void Quitting()
        {
            File.WriteAllText(Path.Combine(Environment.GetEnvironmentVariable("UNTERM_LIFECYCLE_PROOF"), "quitting"), "normal quit");
        }
        [Serializable] private sealed class Identity { public int pid; public string editorVersion; }
    }
}

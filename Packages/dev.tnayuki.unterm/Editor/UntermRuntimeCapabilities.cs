using System;
using System.Reflection;
using System.Collections.Generic;

namespace Unterm.Editor
{
    internal static class UntermRuntimeCapabilities
    {
        private static readonly bool IsMono = typeof(object).Assembly.GetType("Mono.Runtime") != null;
        internal static bool IsCoreCLR => !IsMono && string.Equals(
            typeof(object).Assembly.GetName().Name, "System.Private.CoreLib", StringComparison.Ordinal);
        internal static bool CanExecuteDynamicCode => IsMono;

        internal static IReadOnlyList<Assembly> GetLoadedAssemblies()
        {
#if UNITY_7000_0_OR_NEWER
            if (IsCoreCLR) return UnityEngine.Assemblies.CurrentAssemblies.GetLoadedAssemblies();
#endif
            return IsMono ? AppDomain.CurrentDomain.GetAssemblies() : Array.Empty<Assembly>();
        }
    }
}

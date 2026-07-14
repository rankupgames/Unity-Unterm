using NUnit.Framework;
using Unterm.Editor;

namespace Unterm.Editor.Tests
{
    /// <summary>Policy tests for the final MCP tool name, action, and project trust boundary.</summary>
    public class McpSecurityTests
    {
        [TestCase("unity_editor", "state", (int)UntermToolRisk.ReadOnly)]
        [TestCase("unity_editor", "play", (int)UntermToolRisk.Mutating)]
        [TestCase("unity_scene", "hierarchy", (int)UntermToolRisk.ReadOnly)]
        [TestCase("unity_scene", "save", (int)UntermToolRisk.Mutating)]
        [TestCase("unity_menu", "search", (int)UntermToolRisk.ReadOnly)]
        [TestCase("unity_menu", "execute", (int)UntermToolRisk.Dangerous)]
        [TestCase("unity_package", "list", (int)UntermToolRisk.ReadOnly)]
        [TestCase("unity_package", "add", (int)UntermToolRisk.Dangerous)]
        [TestCase("unity_prefab", "instantiate", (int)UntermToolRisk.Mutating)]
        [TestCase("unity_execute_code", "", (int)UntermToolRisk.Dangerous)]
        public void Classify_UsesFinalAction(string tool, string action, int expected)
        {
            Assert.AreEqual((UntermToolRisk)expected, UntermMcpSecurity.ClassifyAction(tool, action));
        }

        [TestCase(false, (int)UntermMcpAccessPolicy.AllowDangerous, (int)UntermToolRisk.ReadOnly, false, false, true, (int)UntermMcpAuthorization.Deny)]
        [TestCase(true, (int)UntermMcpAccessPolicy.Prompt, (int)UntermToolRisk.ReadOnly, false, false, true, (int)UntermMcpAuthorization.Allow)]
        [TestCase(true, (int)UntermMcpAccessPolicy.Prompt, (int)UntermToolRisk.Mutating, false, false, false, (int)UntermMcpAuthorization.Prompt)]
        [TestCase(true, (int)UntermMcpAccessPolicy.Prompt, (int)UntermToolRisk.Mutating, false, false, true, (int)UntermMcpAuthorization.Deny)]
        [TestCase(true, (int)UntermMcpAccessPolicy.AllowMutating, (int)UntermToolRisk.Mutating, false, false, true, (int)UntermMcpAuthorization.Allow)]
        [TestCase(true, (int)UntermMcpAccessPolicy.AllowMutating, (int)UntermToolRisk.Dangerous, false, false, false, (int)UntermMcpAuthorization.Prompt)]
        [TestCase(true, (int)UntermMcpAccessPolicy.AllowMutating, (int)UntermToolRisk.Dangerous, false, false, true, (int)UntermMcpAuthorization.Deny)]
        [TestCase(true, (int)UntermMcpAccessPolicy.AllowDangerous, (int)UntermToolRisk.Dangerous, false, false, true, (int)UntermMcpAuthorization.Allow)]
        public void ResolveAuthorization_AppliesProjectPolicy(bool enabled, int accessPolicyValue, int riskValue, bool arbitraryCSharp, bool allowArbitraryCSharp, bool unattended, int expectedValue)
        {
            UntermMcpAccessPolicy accessPolicy = (UntermMcpAccessPolicy)accessPolicyValue;
            UntermToolRisk risk = (UntermToolRisk)riskValue;
            UntermMcpAuthorization actual = UntermMcpSecurity.ResolveAuthorization(enabled, accessPolicy, risk, arbitraryCSharp, allowArbitraryCSharp, unattended);
            Assert.AreEqual((UntermMcpAuthorization)expectedValue, actual);
        }

        [TestCase((int)UntermMcpAccessPolicy.Prompt, false, false, (int)UntermMcpAuthorization.Prompt)]
        [TestCase((int)UntermMcpAccessPolicy.Prompt, true, true, (int)UntermMcpAuthorization.Deny)]
        [TestCase((int)UntermMcpAccessPolicy.AllowMutating, true, true, (int)UntermMcpAuthorization.Deny)]
        [TestCase((int)UntermMcpAccessPolicy.AllowDangerous, false, false, (int)UntermMcpAuthorization.Prompt)]
        [TestCase((int)UntermMcpAccessPolicy.AllowDangerous, false, true, (int)UntermMcpAuthorization.Deny)]
        [TestCase((int)UntermMcpAccessPolicy.AllowDangerous, true, true, (int)UntermMcpAuthorization.Allow)]
        public void ExecuteCode_RequiresDangerousPolicyAndSeparateOptInForUnattendedAccess(int accessPolicyValue, bool allowArbitraryCSharp, bool unattended, int expectedValue)
        {
            UntermMcpAccessPolicy accessPolicy = (UntermMcpAccessPolicy)accessPolicyValue;
            UntermMcpAuthorization actual = UntermMcpSecurity.ResolveAuthorization(true, accessPolicy, UntermToolRisk.Dangerous, true, allowArbitraryCSharp, unattended);
            Assert.AreEqual((UntermMcpAuthorization)expectedValue, actual);
        }

        [Test]
        public void UnknownTools_NeverBecomeAutoAllowed()
        {
            UntermToolRisk risk = UntermMcpSecurity.ClassifyAction("unity_future_tool", "");
            Assert.AreEqual(UntermToolRisk.Unclassified, risk);
            Assert.AreEqual(
                UntermMcpAuthorization.Prompt,
                UntermMcpSecurity.ResolveAuthorization(true, UntermMcpAccessPolicy.AllowDangerous, risk, false, true, false));
            Assert.AreEqual(
                UntermMcpAuthorization.Deny,
                UntermMcpSecurity.ResolveAuthorization(true, UntermMcpAccessPolicy.AllowDangerous, risk, false, true, true));
        }

        [Test]
        public void InvalidOrMissingPersistedPolicy_FailsClosedToPrompt()
        {
            Assert.AreEqual(UntermMcpAccessPolicy.Prompt, UntermMcpSecurity.ParseAccessPolicy(null));
            Assert.AreEqual(UntermMcpAccessPolicy.Prompt, UntermMcpSecurity.ParseAccessPolicy("AllowEverything"));
            Assert.AreEqual(UntermMcpAccessPolicy.AllowMutating, UntermMcpSecurity.ParseAccessPolicy("AllowMutating"));
            Assert.AreEqual(
                UntermMcpAuthorization.Deny,
                UntermMcpSecurity.ResolveAuthorization(true, (UntermMcpAccessPolicy)999, UntermToolRisk.Dangerous, false, false, true));
            Assert.AreEqual(
                UntermMcpAuthorization.Prompt,
                UntermMcpSecurity.ResolveAuthorization(true, (UntermMcpAccessPolicy)999, UntermToolRisk.Dangerous, false, false, false));
        }

        [Test]
        public void DisabledCatalogRequest_DoesNotInitializeTools()
        {
            bool initializedBefore = UntermMcpServer.ToolsInitialized;
            Assert.AreEqual("[]", UntermMcpServer.ToolsJson(false));
            Assert.AreEqual(initializedBefore, UntermMcpServer.ToolsInitialized);
        }

        [Test]
        public void StopWithoutNative_ClearsManagedCatalog()
        {
            UntermMcpServer.ToolsJson(true);
            Assert.IsTrue(UntermMcpServer.ToolsInitialized);

            UntermMcpServer.Stop(null);

            Assert.IsFalse(UntermMcpServer.ToolsInitialized);
        }

        [Test]
        public void MissingActions_MatchReadDefaultsAndFailClosedForExecutionDefaults()
        {
            Assert.AreEqual(UntermToolRisk.ReadOnly,
                UntermMcpSecurity.ClassifyAction("unity_asset", ""), "asset defaults to find");
            Assert.AreEqual(UntermToolRisk.ReadOnly,
                UntermMcpSecurity.ClassifyAction("unity_script", ""), "script defaults to read");
            Assert.AreEqual(UntermToolRisk.Dangerous,
                UntermMcpSecurity.ClassifyAction("unity_menu", ""), "menu defaults to execute");
            Assert.AreEqual(UntermToolRisk.Dangerous,
                UntermMcpSecurity.ClassifyAction("unity_material", ""), "material has no read-only default");
            Assert.AreEqual(UntermToolRisk.Mutating,
                UntermMcpSecurity.ClassifyAction("unity_prefab", ""), "prefab defaults to instantiate");
        }
    }
}

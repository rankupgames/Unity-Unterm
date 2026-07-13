using Newtonsoft.Json.Linq;
using NUnit.Framework;
using Unterm.Editor;

namespace Unterm.Editor.Tests
{
    /// <summary>Policy tests for the final MCP tool name and action boundary.</summary>
    public class McpSecurityTests
    {
        [TestCase("unity_editor", "state", UntermToolRisk.ReadOnly)]
        [TestCase("unity_editor", "play", UntermToolRisk.Mutating)]
        [TestCase("unity_scene", "hierarchy", UntermToolRisk.ReadOnly)]
        [TestCase("unity_scene", "save", UntermToolRisk.Mutating)]
        [TestCase("unity_menu", "search", UntermToolRisk.ReadOnly)]
        [TestCase("unity_menu", "execute", UntermToolRisk.Dangerous)]
        [TestCase("unity_package", "list", UntermToolRisk.ReadOnly)]
        [TestCase("unity_package", "add", UntermToolRisk.Dangerous)]
        [TestCase("unity_prefab", "instantiate", UntermToolRisk.Mutating)]
        public void Classify_UsesFinalAction(string tool, string action, UntermToolRisk expected)
        {
            Assert.AreEqual(expected, UntermMcpSecurity.Classify(tool, new JObject { ["action"] = action }));
        }

        [Test]
        public void ExecuteCode_IsAlwaysDangerousAndInteractive()
        {
            var risk = UntermMcpSecurity.Classify("unity_execute_code", new JObject());
            Assert.AreEqual(UntermToolRisk.Dangerous, risk);
            Assert.IsTrue(UntermMcpSecurity.RequiresOneShotApproval(risk));
            Assert.IsFalse(UntermMcpSecurity.CanRunInBatchMode(risk));
        }

        [Test]
        public void UnknownTools_FailClosed()
        {
            Assert.AreEqual(UntermToolRisk.Dangerous,
                UntermMcpSecurity.Classify("unity_future_tool", new JObject()));
        }

        [Test]
        public void MissingActions_MatchReadDefaultsAndFailClosedForExecutionDefaults()
        {
            Assert.AreEqual(UntermToolRisk.ReadOnly,
                UntermMcpSecurity.Classify("unity_asset", new JObject()), "asset defaults to find");
            Assert.AreEqual(UntermToolRisk.ReadOnly,
                UntermMcpSecurity.Classify("unity_script", new JObject()), "script defaults to read");
            Assert.AreEqual(UntermToolRisk.Dangerous,
                UntermMcpSecurity.Classify("unity_menu", new JObject()), "menu defaults to execute");
            Assert.AreEqual(UntermToolRisk.Dangerous,
                UntermMcpSecurity.Classify("unity_material", new JObject()), "material has no read-only default");
            Assert.AreEqual(UntermToolRisk.Mutating,
                UntermMcpSecurity.Classify("unity_prefab", new JObject()), "prefab defaults to instantiate");
        }
    }
}

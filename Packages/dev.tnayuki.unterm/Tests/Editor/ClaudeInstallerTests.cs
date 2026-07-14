using System;
using System.IO;
using NUnit.Framework;
using Unterm.Editor;

namespace Unterm.Editor.Tests
{
    /// <summary>Focused tests for passive Claude executable discovery.</summary>
    public class ClaudeInstallerTests
    {
        private string _root;

        [SetUp]
        public void SetUp()
        {
            _root = Path.Combine(Path.GetTempPath(), "unterm-claude-locator-tests-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(_root);
        }

        [TearDown]
        public void TearDown()
        {
            if (Directory.Exists(_root)) Directory.Delete(_root, true);
        }

        [Test]
        public void FindOnPath_ReturnsExistingClaudeExecutable()
        {
            string binary = Path.Combine(_root, UntermClaudeInstaller.BinaryName);
            File.WriteAllText(binary, "test executable");

            Assert.AreEqual(Path.GetFullPath(binary), UntermClaudeInstaller.FindOnPath(_root));
        }

        [Test]
        public void FindOnPath_SkipsMissingDirectories()
        {
            string binary = Path.Combine(_root, UntermClaudeInstaller.BinaryName);
            File.WriteAllText(binary, "test executable");
            string pathValue = Path.Combine(_root, "missing") + Path.PathSeparator + _root;

            Assert.AreEqual(Path.GetFullPath(binary), UntermClaudeInstaller.FindOnPath(pathValue));
        }

        [Test]
        public void FindOnPath_ReturnsEmptyWhenClaudeIsMissing()
        {
            Assert.AreEqual(string.Empty, UntermClaudeInstaller.FindOnPath(_root));
        }
    }
}

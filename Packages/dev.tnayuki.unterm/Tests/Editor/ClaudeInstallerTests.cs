using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Text;
using NUnit.Framework;
using Unterm.Editor;

namespace Unterm.Editor.Tests
{
    /// <summary>Archive-boundary tests for the pinned Claude Code installer.</summary>
    public class ClaudeInstallerTests
    {
        private string _root;

        [SetUp]
        public void SetUp()
        {
            _root = Path.Combine(Path.GetTempPath(), "unterm-installer-tests-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(_root);
        }

        [TearDown]
        public void TearDown()
        {
            if (Directory.Exists(_root)) Directory.Delete(_root, true);
        }

        [Test]
        public void ExtractBinary_AcceptsReviewedLayout()
        {
            string archive = WriteArchive(ValidEntries());
            string destination = Path.Combine(_root, UntermClaudeInstaller.BinaryName);

            Assert.IsNull(UntermClaudeInstaller.ExtractBinary(archive, destination));
            Assert.AreEqual("reviewed-binary", File.ReadAllText(destination));
        }

        [Test]
        public void ExtractBinary_RejectsTraversalAndRemovesStagingFile()
        {
            var entries = ValidEntries();
            entries.Add(new Entry("package/../escape", '0', "bad"));
            string archive = WriteArchive(entries);
            string destination = Path.Combine(_root, UntermClaudeInstaller.BinaryName);

            StringAssert.Contains("unsafe archive path", UntermClaudeInstaller.ExtractBinary(archive, destination));
            Assert.IsFalse(File.Exists(destination));
            Assert.IsFalse(File.Exists(destination + ".extracting"));
        }

        [Test]
        public void ExtractBinary_RejectsSymlink()
        {
            var entries = ValidEntries();
            entries[0] = new Entry("package/" + UntermClaudeInstaller.BinaryName, '2', "");
            string archive = WriteArchive(entries);

            StringAssert.Contains("non-regular", UntermClaudeInstaller.ExtractBinary(
                archive, Path.Combine(_root, UntermClaudeInstaller.BinaryName)));
        }

        [Test]
        public void ExtractBinary_RejectsUnexpectedLayout()
        {
            var entries = ValidEntries();
            entries.Add(new Entry("package/postinstall.js", '0', "unexpected"));
            string archive = WriteArchive(entries);

            StringAssert.Contains("unexpected archive entry", UntermClaudeInstaller.ExtractBinary(
                archive, Path.Combine(_root, UntermClaudeInstaller.BinaryName)));
        }

        [Test]
        public void ExtractBinary_RejectsOversizedMetadataBeforeReadingPayload()
        {
            var entries = ValidEntries();
            entries[3] = new Entry("package/README.md", '0', "", 2L * 1024L * 1024L);
            string archive = WriteArchive(entries);

            StringAssert.Contains("too large", UntermClaudeInstaller.ExtractBinary(
                archive, Path.Combine(_root, UntermClaudeInstaller.BinaryName)));
        }

        private static List<Entry> ValidEntries() => new List<Entry>
        {
            new Entry("package/" + UntermClaudeInstaller.BinaryName, '0', "reviewed-binary"),
            new Entry("package/package.json", '0', "{}"),
            new Entry("package/LICENSE.md", '0', "license"),
            new Entry("package/README.md", '0', "readme"),
        };

        private string WriteArchive(IReadOnlyList<Entry> entries)
        {
            string path = Path.Combine(_root, "fixture.tgz");
            using (var file = new FileStream(path, FileMode.CreateNew, FileAccess.Write))
            using (var gzip = new GZipStream(file, CompressionMode.Compress))
            {
                foreach (Entry entry in entries) WriteEntry(gzip, entry);
                gzip.Write(new byte[1024], 0, 1024);
            }
            return path;
        }

        private static void WriteEntry(Stream stream, Entry entry)
        {
            byte[] content = Encoding.UTF8.GetBytes(entry.Content);
            long declaredSize = entry.DeclaredSize ?? content.Length;
            var header = new byte[512];
            WriteAscii(header, 0, 100, entry.Name);
            WriteOctal(header, 100, 8, 493);
            WriteOctal(header, 108, 8, 0);
            WriteOctal(header, 116, 8, 0);
            WriteOctal(header, 124, 12, declaredSize);
            WriteOctal(header, 136, 12, 0);
            for (int i = 148; i < 156; i++) header[i] = 32;
            header[156] = (byte)entry.TypeFlag;
            WriteAscii(header, 257, 6, "ustar");
            long checksum = 0;
            foreach (byte value in header) checksum += value;
            WriteOctal(header, 148, 8, checksum);
            stream.Write(header, 0, header.Length);

            if (entry.DeclaredSize.HasValue && entry.DeclaredSize.Value != content.Length) return;
            stream.Write(content, 0, content.Length);
            int padding = (int)((512 - (content.Length % 512)) % 512);
            if (padding > 0) stream.Write(new byte[padding], 0, padding);
        }

        private static void WriteAscii(byte[] buffer, int offset, int length, string value)
        {
            byte[] bytes = Encoding.ASCII.GetBytes(value);
            Array.Copy(bytes, 0, buffer, offset, Math.Min(length, bytes.Length));
        }

        private static void WriteOctal(byte[] buffer, int offset, int length, long value)
        {
            string text = Convert.ToString(value, 8).PadLeft(length - 1, '0');
            WriteAscii(buffer, offset, length - 1, text);
            buffer[offset + length - 1] = 0;
        }

        private readonly struct Entry
        {
            public readonly string Name;
            public readonly char TypeFlag;
            public readonly string Content;
            public readonly long? DeclaredSize;

            public Entry(string name, char typeFlag, string content, long? declaredSize = null)
            {
                Name = name;
                TypeFlag = typeFlag;
                Content = content;
                DeclaredSize = declaredSize;
            }
        }
    }
}

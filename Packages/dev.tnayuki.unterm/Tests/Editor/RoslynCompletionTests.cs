using System.Collections;
using System.Collections.Generic;
using UnityEngine.TestTools;
using System.Linq;
using NUnit.Framework;
using Unterm.Editor;

namespace Unterm.Editor.Tests
{
    /// <summary>
    /// EditMode tests for the Roslyn-backed C# completion engine
    /// (<see cref="UntermRoslynCompletion"/>). This is the part that decides WHICH
    /// items to suggest for a given source + caret — the Rust side only renders the
    /// popup and applies the accepted item (covered by CodeEditorTests).
    ///
    /// One test per completion context documented in Assets/CompletionPlayground.cs:
    /// member, scope/general, attribute, new/type, named-argument, expected-enum,
    /// object-initializer, using/namespace, unimported-type + auto-using, override,
    /// and signature help.
    ///
    /// No native bundle or GPU is needed: the engine is pure Roslyn over source text,
    /// with the reference set built from <c>CompilationPipeline</c> (a Unity
    /// main-thread API, hence EditMode). Methods take (text, caretIndex) and return
    /// (insert, label, kind) tuples.
    /// </summary>
    public class RoslynCompletionTests
    {
        [OneTimeSetUp]
        public void BuildReferences()
        {
            // CompilationPipeline is main-thread only; EditMode tests run on it.
            UntermRoslynCompletion.EnsureReferences();
        }

        private static List<string> Inserts(List<(string insert, string label, char kind)> items)
        {
            Assert.IsNotNull(items, "completion returned null");
            return items.Select(i => i.insert).ToList();
        }

        // Caret index just after the given (unique) substring within src.
        private static int After(string src, string needle) => src.IndexOf(needle) + needle.Length;

        // 1) MEMBER — static type access surfaces static members.
        [Test]
        public void Member_StaticType_OffersStaticMembers()
        {
            var src = "class C { void M() { System.Console. } }";
            var inserts = Inserts(UntermRoslynCompletion.MemberCompletions(src, After(src, "System.Console.")));
            CollectionAssert.Contains(inserts, "WriteLine");
            CollectionAssert.Contains(inserts, "ReadLine");
        }

        // 1b) MEMBER — instance access, with synthesized accessors/ctors filtered out.
        [Test]
        public void Member_Instance_FiltersAccessorsAndCtors()
        {
            var src = "class C { void M() { \"hi\". } }";
            var inserts = Inserts(UntermRoslynCompletion.MemberCompletions(src, After(src, "\"hi\".")));
            CollectionAssert.Contains(inserts, "Substring");
            Assert.IsFalse(inserts.Any(n => n.StartsWith("get_") || n.StartsWith("set_") || n.StartsWith(".")),
                "accessor/ctor synthetic members leaked into completions");
        }

        // 2) GENERAL — locals in scope appear.
        [Test]
        public void General_IncludesLocalsInScope()
        {
            var src = "class C { void M() { int local = 0; var y =  } }";
            var inserts = Inserts(UntermRoslynCompletion.GeneralCompletions(src, After(src, "var y = ")));
            CollectionAssert.Contains(inserts, "local");
        }

        // 2b) GENERAL — imported type by simple name.
        [Test]
        public void General_IncludesImportedTypeBySimpleName()
        {
            var src = "using System; class C { void M() { var y =  } }";
            var inserts = Inserts(UntermRoslynCompletion.GeneralCompletions(src, After(src, "var y = ")));
            CollectionAssert.Contains(inserts, "Console");
        }

        // MEMBER negative — not a member access yields null.
        [Test]
        public void Member_NotAMemberAccess_ReturnsNull()
        {
            var src = "class C { void M() { int x = 1; } }";
            Assert.IsNull(UntermRoslynCompletion.MemberCompletions(src, After(src, "int x = 1")));
        }

        // 3) ATTRIBUTE — after `[`, only Attribute types (suffix trimmed), not values.
        [Test]
        public void Attribute_OffersAttributeTypesTrimmed()
        {
            var src = "using System; class C { [] int f; }";
            var inserts = Inserts(UntermRoslynCompletion.AttributeCompletions(src, After(src, "[")));
            CollectionAssert.Contains(inserts, "Serializable"); // SerializableAttribute -> "Serializable"
            CollectionAssert.DoesNotContain(inserts, "Console"); // non-attribute types excluded
        }

        // 4) NEW — types only (no locals).
        [Test]
        public void Type_New_OffersTypesNotLocals()
        {
            var src = "using System; class C { void M() { int local = 0; var x = new  } }";
            var inserts = Inserts(UntermRoslynCompletion.TypeCompletions(src, After(src, "new ")));
            CollectionAssert.Contains(inserts, "Exception");
            CollectionAssert.DoesNotContain(inserts, "local");
        }

        // 5) NAMED ARGUMENTS — inside a call, parameter names as `name: ` items.
        [Test]
        public void NamedArguments_OfferParameterNames()
        {
            var src = "class C { void Foo(int alpha, int beta) {} void M() { Foo(); } }";
            int pos = src.IndexOf("Foo();") + 4; // between '(' and ')'
            var inserts = Inserts(UntermRoslynCompletion.GeneralCompletions(src, pos));
            CollectionAssert.Contains(inserts, "alpha: ");
            CollectionAssert.Contains(inserts, "beta: ");
        }

        // 6) ENUM value — where an enum is expected, qualified members lead.
        [Test]
        public void ExpectedEnum_OffersQualifiedMembers()
        {
            var src = "class C { enum E { Idle, Firing } void M() { E p =  } }";
            var inserts = Inserts(UntermRoslynCompletion.GeneralCompletions(src, After(src, "E p = ")));
            CollectionAssert.Contains(inserts, "E.Idle");
            CollectionAssert.Contains(inserts, "E.Firing");
        }

        // 7) OBJECT INITIALIZER — settable members as `Name = `.
        [Test]
        public void ObjectInitializer_OffersSettableMembers()
        {
            var src = "class P { public int X { get; set; } public int Y; } "
                    + "class C { void M() { var r = new P {  } } }";
            var inserts = Inserts(UntermRoslynCompletion.GeneralCompletions(src, After(src, "new P { ")));
            CollectionAssert.Contains(inserts, "X = ");
            CollectionAssert.Contains(inserts, "Y = ");
        }

        // 8) USING — namespaces only (types excluded).
        [Test]
        public void Namespace_OffersNamespacesNotTypes()
        {
            var src = "class C { void M() {  } }";
            var inserts = Inserts(UntermRoslynCompletion.NamespaceCompletions(src, After(src, "void M() { ")));
            CollectionAssert.Contains(inserts, "System");
            CollectionAssert.DoesNotContain(inserts, "Console"); // a type, not a namespace
        }

        // 9) UNIMPORTED TYPE — prefix match across all references, carrying its namespace.
        [Test]
        public void UnimportedTypes_MatchByPrefixWithNamespace()
        {
            // Build the cross-assembly type index (populated as a side effect of a
            // GeneralCompletions call), then query it directly.
            UntermRoslynCompletion.GeneralCompletions("class C { void M() {  } }", 20);

            var items = UntermRoslynCompletion.UnimportedTypesMatching("StringBui", new HashSet<string>());
            Assert.IsNotNull(items, "type index not built");
            var hit = items.FirstOrDefault(i => i.insert == "StringBuilder");
            Assert.AreEqual("StringBuilder", hit.insert, "StringBuilder not offered");
            StringAssert.Contains("System.Text", hit.label); // label is `Name  (Namespace)`
        }

        // 9b) The namespace is recoverable from the label for the auto-`using` insert.
        [Test]
        public void NamespaceFromUnimportedLabel_Parses()
        {
            Assert.AreEqual("System.Collections.Generic",
                UntermRoslynCompletion.NamespaceFromUnimportedLabel("List  (System.Collections.Generic)"));
            Assert.IsNull(UntermRoslynCompletion.NamespaceFromUnimportedLabel("NoParens"));
        }

        // 10) OVERRIDE — base virtual members as ready-to-paste signatures.
        [Test]
        public void Override_OffersBaseVirtualMembers()
        {
            var src = "class Base { public virtual void Speak() {} } "
                    + "class Derived : Base { override  }";
            var items = UntermRoslynCompletion.OverrideCompletions(src, After(src, "override "));
            Assert.IsNotNull(items);
            Assert.IsTrue(items.Any(i => i.label.Contains("Speak")), "Speak not offered for override");
            Assert.IsTrue(items.Any(i => i.insert.Contains("base.Speak(")), "override body should call base");
        }

        // 11) SIGNATURE HELP — the surrounding call's parameters with the active one.
        [Test]
        public void SignatureHelp_ReportsParameters()
        {
            var src = "class C { void Foo(int alpha, int beta) {} void M() { Foo(); } }";
            int pos = src.IndexOf("Foo();") + 4; // inside the parentheses
            var help = UntermRoslynCompletion.SignatureHelp(src, pos);
            Assert.IsNotNull(help, "no signature help inside call");
            Assert.GreaterOrEqual(help.Items.Count, 1);
            CollectionAssert.Contains(help.Items[0].Parameters, "int alpha");
        }


        [Test]
        public void ExecuteCode_CoreCLR_RefusesBeforeCompilation()
        {
            if (!UntermRuntimeCapabilities.IsCoreCLR)
                Assert.Ignore("This capability refusal requires an actual CoreCLR Editor.");
            System.Func<Newtonsoft.Json.Linq.JObject, object> execute = null;
            new UntermExecuteCodeTools().Register(new UntermToolSink(
                (name, description, schema, handler) => { if (name == "unity_execute_code") execute = handler; }));
            Assert.IsNotNull(execute);
            var result = Newtonsoft.Json.Linq.JObject.FromObject(execute(
                new Newtonsoft.Json.Linq.JObject { ["code"] = "deliberately invalid C# source" }));
            Assert.IsFalse((bool)result["ok"]);
            StringAssert.StartsWith("capability_unavailable:", (string)result["error"]);
            Assert.IsNull(result["diagnostics"], "Roslyn compiled code on an unavailable runtime.");
        }

        [UnityTest]
        public IEnumerator CompletionWorker_StopAndReinitialize_Restarts()
        {
            yield return WorkerRestarts(typeof(UntermCompletionWorker));
        }

        [UnityTest]
        public IEnumerator SignatureWorker_StopAndReinitialize_Restarts()
        {
            yield return WorkerRestarts(typeof(UntermSignatureWorker));
        }

        [UnityTest]
        public IEnumerator CompletionWorker_Stop_DiscardsUnreadResult()
        {
            yield return WorkerDiscardsResult(typeof(UntermCompletionWorker));
        }

        [UnityTest]
        public IEnumerator SignatureWorker_Stop_DiscardsUnreadResult()
        {
            yield return WorkerDiscardsResult(typeof(UntermSignatureWorker));
        }

        private static IEnumerator WorkerRestarts(System.Type worker)
        {
            // Initial, never-started and repeated shutdowns must all be safe.
            InvokeWorker(worker, "Shutdown");
            InvokeWorker(worker, "Shutdown");
            InvokeWorker(worker, "RegisterShutdown");
            InvokeWorker(worker, "RegisterShutdown");
            UntermRoslynCompletion.EnsureReferences();
            try
            {
                object first = null;
                long sequence = SubmitWorker(worker);
                yield return AwaitWorker(() => HasWorkerResult(worker, sequence));
                Assert.IsTrue(TakeWorker(worker, sequence, out first));
                AssertWorkerResult(worker, first);
                InvokeWorker(worker, "Shutdown");
                InvokeWorker(worker, "Shutdown");
                InvokeWorker(worker, "RegisterShutdown");
                object second = null;
                sequence = SubmitWorker(worker);
                yield return AwaitWorker(() => HasWorkerResult(worker, sequence));
                Assert.IsTrue(TakeWorker(worker, sequence, out second));
                AssertWorkerResult(worker, second);
            }
            finally
            {
                InvokeWorker(worker, "Shutdown");
                InvokeWorker(worker, "RegisterShutdown");
            }
        }

        private static IEnumerator WorkerDiscardsResult(System.Type worker)
        {
            InvokeWorker(worker, "Shutdown");
            InvokeWorker(worker, "RegisterShutdown");
            UntermRoslynCompletion.EnsureReferences();
            try
            {
                long sequence = SubmitWorker(worker);
                var resultSequence = worker.GetField("s_resultSeq",
                    System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic);
                // Wait for a real unread semantic result, then observe the public polling boundary.
                yield return AwaitWorker(() => (long)resultSequence.GetValue(null) == sequence);
                AssertWorkerResult(worker, worker.GetField("s_result",
                    System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic).GetValue(null));
                InvokeWorker(worker, "Shutdown");
                Assert.IsFalse(TakeWorker(worker, sequence, out _),
                    "A stopped worker published an unread result from its previous lifecycle.");
            }
            finally
            {
                InvokeWorker(worker, "Shutdown");
                InvokeWorker(worker, "RegisterShutdown");
            }
        }

        private static long SubmitWorker(System.Type worker)
        {
            if (worker == typeof(UntermCompletionWorker))
            {
                const string source = "class C { void M() { System.Console. } }";
                return UntermCompletionWorker.Submit(source, After(source, "System.Console."), 1);
            }
            const string signature = "class C { void Foo(int alpha) {} void M() { Foo(); } }";
            return UntermSignatureWorker.Submit(signature, signature.IndexOf("Foo();") + 4);
        }

        private static bool HasWorkerResult(System.Type worker, long sequence) =>
            (long)worker.GetField("s_resultSeq", System.Reflection.BindingFlags.Static |
                System.Reflection.BindingFlags.NonPublic).GetValue(null) == sequence;

        private static bool TakeWorker(System.Type worker, long sequence, out object result)
        {
            if (worker == typeof(UntermCompletionWorker))
            {
                bool ready = UntermCompletionWorker.TryTake(sequence, out var completion);
                result = completion;
                return ready;
            }
            bool signatureReady = UntermSignatureWorker.TryTake(sequence, out var signature);
            result = signature;
            return signatureReady;
        }

        private static void AssertWorkerResult(System.Type worker, object result)
        {
            Assert.IsNotNull(result, "The worker returned unavailable after reinitialization: accepting=" +
                worker.GetField("s_accepting", System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic)?.GetValue(null) +
                ", stop=" + worker.GetField("s_stop", System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic).GetValue(null) +
                ", thread=" + worker.GetField("s_thread", System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic).GetValue(null));
            if (worker == typeof(UntermCompletionWorker))
                CollectionAssert.Contains(Inserts((List<(string insert, string label, char kind)>)result), "WriteLine");
            else
                CollectionAssert.Contains(((UntermRoslynCompletion.SigHelp)result).Items[0].Parameters, "int alpha");
        }

        private static IEnumerator AwaitWorker(System.Func<bool> ready)
        {
            var timer = System.Diagnostics.Stopwatch.StartNew();
            while (!ready())
            {
                Assert.Less(timer.ElapsedMilliseconds, 10000, "Worker result exceeded the bounded deadline.");
                yield return null;
            }
        }

        private static void InvokeWorker(System.Type worker, string method)
        {
            worker.GetMethod(method, System.Reflection.BindingFlags.Static |
                System.Reflection.BindingFlags.NonPublic).Invoke(null, null);
        }
    }
}

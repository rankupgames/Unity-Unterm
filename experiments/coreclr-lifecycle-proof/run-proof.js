'use strict';
const fs = require('node:fs'), path = require('node:path'), os = require('node:os'), crypto = require('node:crypto');
const { spawn, spawnSync } = require('node:child_process');
const option = name => { const i = process.argv.indexOf(name); return i < 0 ? '' : process.argv[i + 1] || ''; };
const editor = option('--editor'), toolkit = option('--toolkit'), version = option('--editor-version'), variant = option('--variant');
if (!editor || !toolkit || !['6000.3.9f1', '7000.0.0a7'].includes(version) || !['original', 'unread-result', 'fixed'].includes(variant)) throw Error('Supply exact reviewed Editor, toolkit root and proof variant.');
const repo = path.resolve(__dirname, '../..'), source = path.join(repo, 'Packages/dev.tnayuki.unterm'), pin = '7caf244b213b62d95d2d76ad480d22c31778994b';
const product = spawnSync('powershell.exe', ['-NoProfile', '-Command', '(Get-Item -LiteralPath $env:UNTERM_PROOF_EDITOR).VersionInfo.ProductVersion'], { encoding: 'utf8', env: { ...process.env, UNTERM_PROOF_EDITOR: editor } });
const metadata = product.stdout.trim();
if (product.status || !metadata.startsWith(version + '_') || !/_[0-9a-f]{12}$/.test(metadata)) throw Error('Exact Editor metadata mismatch.');
const builtin = path.join(path.dirname(editor), 'Data/Resources/PackageManager/BuiltInPackages');
function localPackage(name, directory) {
  for (const entry of fs.readdirSync(directory).filter(entry => entry === name || entry.startsWith(name + '@'))) {
    const root = path.join(directory, entry), file = path.join(root, 'package.json');
    if (fs.existsSync(file) && JSON.parse(fs.readFileSync(file, 'utf8')).name === name) return root;
  }
  throw Error('Required existing local package unavailable: ' + name);
}
const tf = localPackage('com.unity.test-framework', builtin), nunit = localPackage('com.unity.ext.nunit', builtin);
const newtonsoft = localPackage('com.unity.nuget.newtonsoft-json', path.join(toolkit, 'CursorUnityTool/Library/PackageCache'));
const vendor = path.join(toolkit, 'Packages/com.rankupgames.unity-cursor-toolkit/Editor/ThirdParty/Unity-Unterm');
const provenance = JSON.parse(fs.readFileSync(path.join(vendor, 'VENDOR.json'), 'utf8'));
if (provenance.sourceCommit !== pin) throw Error('Reviewed vendor source pin mismatch.');
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const assets = ['Plugins/Roslyn/Microsoft.CodeAnalysis.CSharp.dll', 'Plugins/Roslyn/Microsoft.CodeAnalysis.dll', 'Plugins/Roslyn/System.Collections.Immutable.dll', 'Plugins/Roslyn/System.Reflection.Metadata.dll', 'Plugins/Windows/x86_64/unterm.dll'];
for (const asset of assets) if (hash(fs.readFileSync(path.join(vendor, asset))) !== provenance.files[asset]) throw Error('Reviewed fixture artifact hash mismatch: ' + asset);
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'unterm-lifecycle-' + variant + '-')), proof = path.join(root, 'proof');
for (const relative of ['Assets/Editor', 'Packages', 'ProjectSettings', 'proof']) fs.mkdirSync(path.join(root, relative), { recursive: true });
const packageRoot = path.join(root, 'Packages/dev.tnayuki.unterm');
fs.cpSync(source, packageRoot, { recursive: true });
for (const asset of assets) {
  fs.mkdirSync(path.dirname(path.join(packageRoot, 'Editor', asset)), { recursive: true });
  for (const suffix of ['', '.meta']) fs.copyFileSync(path.join(vendor, asset + suffix), path.join(packageRoot, 'Editor', asset + suffix));
}
// Mono's bundled vendor Roslyn metadata needs Unsafe6; use only the selected Editor's existing compiler artifact.
let fixtureDependency = null;
if (version === '6000.3.9f1') {
  const relative = 'Data/NetStandard/EditorExtensions/System.Runtime.CompilerServices.Unsafe.dll';
  const unsafeSource = path.join(path.dirname(editor), relative), target = path.join(packageRoot, 'Editor/Plugins/Roslyn/System.Runtime.CompilerServices.Unsafe.dll');
  const name = spawnSync('powershell.exe', ['-NoProfile', '-Command', '[System.Reflection.AssemblyName]::GetAssemblyName($env:UNTERM_UNSAFE_SOURCE).FullName'], { encoding: 'utf8', env: { ...process.env, UNTERM_UNSAFE_SOURCE: unsafeSource } });
  if (name.status || name.stdout.trim() !== 'System.Runtime.CompilerServices.Unsafe, Version=6.0.0.0, Culture=neutral, PublicKeyToken=b03f5f7f11d50a3a') throw Error('Existing Mono fixture dependency mismatch.');
  fs.copyFileSync(unsafeSource, target);
  fs.writeFileSync(target + '.meta', fs.readFileSync(path.join(vendor, 'Plugins/Roslyn/System.Collections.Immutable.dll.meta'), 'utf8').replace(/guid: [a-f0-9]+/, 'guid: ' + crypto.randomBytes(16).toString('hex')));
  const asmdef = path.join(packageRoot, 'Editor/Unterm.Editor.asmdef'), data = JSON.parse(fs.readFileSync(asmdef, 'utf8'));
  data.precompiledReferences.push('System.Runtime.CompilerServices.Unsafe.dll'); fs.writeFileSync(asmdef, JSON.stringify(data, null, 4));
  fixtureDependency = { source: '<EDITOR>/' + relative, assembly: name.stdout.trim(), sha256: hash(fs.readFileSync(unsafeSource)), fixtureOnly: true };
}
// Fixture-only diagnostics expose the engine's existing swallowed failures; no behavior override.
const roslynFile = path.join(packageRoot, 'Editor/UntermRoslynCompletion.cs');
let roslynBody = fs.readFileSync(roslynFile, 'utf8').replace(/\r\n/g, '\n');
const report = 'catch (Exception exception) { try { File.AppendAllText(Path.Combine(Environment.GetEnvironmentVariable("UNTERM_LIFECYCLE_PROOF"), "roslyn-errors"), exception.GetType().Name + ": " + exception.Message + Environment.NewLine); } catch {} return null; }';
const first = roslynBody.indexOf('            catch\n            {\n                return null;\n            }');
if (first < 0 || !roslynBody.includes('            catch { return null; }')) throw Error('Roslyn diagnostic boundaries unavailable.');
roslynBody = roslynBody.slice(0, first) + '            ' + report + roslynBody.slice(first + '            catch\n            {\n                return null;\n            }'.length);
roslynBody = roslynBody.replace('            catch { return null; }', '            ' + report);
fs.writeFileSync(roslynFile, roslynBody);
const changed = ['UntermCompletionWorker.cs', 'UntermSignatureWorker.cs', 'UntermExecuteCodeTools.cs'];
if (variant === 'original') for (const name of changed) {
  const old = spawnSync('git', ['-C', repo, 'show', pin + ':Packages/dev.tnayuki.unterm/Editor/' + name], { encoding: 'utf8' });
  if (old.status !== 0) throw Error('Original source object unavailable.');
  fs.writeFileSync(path.join(packageRoot, 'Editor', name), old.stdout);
}
if (variant === 'unread-result') for (const name of changed.slice(0, 2)) {
  const file = path.join(packageRoot, 'Editor', name), body = fs.readFileSync(file, 'utf8').replace(/\r\n/g, '\n');
  const clear = '                s_hasPending = false;\n                s_result = null;\n                s_resultSeq = -1;\n                thread = s_thread;';
  if (!body.includes(clear)) throw Error('Isolated unread-result mutation boundary absent.');
  fs.writeFileSync(file, body.replace(clear, '                s_hasPending = false;\n                thread = s_thread;'));
}
const dependencies = { 'dev.tnayuki.unterm': 'file:dev.tnayuki.unterm' }, packageVersions = [];
for (const directory of [tf, nunit, newtonsoft]) {
  const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
  fs.cpSync(directory, path.join(root, 'Packages', manifest.name), { recursive: true });
  dependencies[manifest.name] = 'file:' + manifest.name;
  packageVersions.push({ name: manifest.name, version: manifest.version, source: directory.startsWith(builtin) ? 'selected_editor_builtin' : 'existing_owned_sample_cache' });
}
for (const name of fs.readdirSync(builtin).filter(name => name.startsWith('com.unity.modules.') && fs.existsSync(path.join(builtin, name, 'package.json')))) dependencies[name] = JSON.parse(fs.readFileSync(path.join(builtin, name, 'package.json'), 'utf8')).version;
fs.writeFileSync(path.join(root, 'Packages/manifest.json'), JSON.stringify({ dependencies, testables: ['dev.tnayuki.unterm'] }, null, 2));
fs.writeFileSync(path.join(root, 'ProjectSettings/ProjectVersion.txt'), 'm_EditorVersion: ' + version + '\nm_EditorVersionWithRevision: ' + version + ' (' + metadata.split('_')[1] + ')\n');
fs.copyFileSync(path.join(__dirname, 'ShutdownMarker.cs'), path.join(root, 'Assets/Editor/ShutdownMarker.cs'));
const methods = variant === 'unread-result' ? ['CompletionWorker_Stop_DiscardsUnreadResult', 'SignatureWorker_Stop_DiscardsUnreadResult'] : ['Member_StaticType_OffersStaticMembers', 'SignatureHelp_ReportsParameters', 'CompletionWorker_StopAndReinitialize_Restarts', 'SignatureWorker_StopAndReinitialize_Restarts', 'CompletionWorker_Stop_DiscardsUnreadResult', 'SignatureWorker_Stop_DiscardsUnreadResult', 'ExecuteCode_CoreCLR_RefusesBeforeCompilation'];
const filter = '^Unterm\\.Editor\\.Tests\\.RoslynCompletionTests\\.(' + methods.join('|') + ')$';
const args = ['-batchmode', '-nographics', '-projectPath', root, '-runTests', '-testPlatform', 'EditMode', '-testFilter', filter, '-testResults', path.join(proof, 'results.xml'), '-logFile', path.join(proof, 'editor.log')];
const prepared = { editorVersion: version, editorMetadata: metadata, variant, packageVersions, vendorPin: pin, fixtureDependency, artifactHashes: Object.fromEntries(assets.map(asset => [asset, provenance.files[asset]])), selectedTests: methods, fixtureSourceHashes: Object.fromEntries([...changed.map(name => 'Editor/' + name), 'Editor/UntermRuntimeCapabilities.cs', 'Editor/UntermRoslynCompletion.cs', 'Editor/Unterm.Editor.asmdef', 'Tests/Editor/RoslynCompletionTests.cs', 'Tests/Editor/Unterm.Editor.Tests.asmdef'].map(name => [name, hash(fs.readFileSync(path.join(packageRoot, name)))])), fixtureOnlyRoslynDiagnostics: true, shutdownMarkerHash: hash(fs.readFileSync(path.join(root, 'Assets/Editor/ShutdownMarker.cs'))) };
if (process.argv.includes('--prepare-only')) { fs.writeFileSync(path.join(proof, 'prepared.json'), JSON.stringify(prepared, null, 2)); console.log(JSON.stringify({ prepared: true, fixture: root, variant })); process.exit(0); }
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
function ownedProcesses() {
  const result = spawnSync('powershell.exe', ['-NoProfile', '-Command', 'Get-CimInstance Win32_Process | Where-Object { $_.Name -match "^Unity" -and $_.CommandLine -and $_.CommandLine.Contains($env:UNTERM_PROOF_ROOT) } | Select-Object -ExpandProperty ProcessId | ConvertTo-Json -Compress'], { encoding: 'utf8', env: { ...process.env, UNTERM_PROOF_ROOT: root } });
  if (result.status !== 0) throw Error('Owned process query failed.');
  const value = result.stdout.trim() ? JSON.parse(result.stdout) : [];
  return Array.isArray(value) ? value : [value];
}
async function execute() {
  const child = spawn(editor, args, { env: { ...process.env, UNTERM_LIFECYCLE_PROOF: proof }, stdio: 'ignore', windowsHide: true });
  let exited = false, exitCode = null, spawnError = '';
  child.on('error', error => { exited = true; spawnError = error.code || 'spawn_error'; });
  child.on('exit', code => { exited = true; exitCode = code; });
  if (child.pid) fs.writeFileSync(path.join(proof, 'owner.pid'), String(child.pid));
  console.log(JSON.stringify({ phase: 'launched', variant, editorVersion: version, pid: child.pid }));
  const start = Date.now(); let readyAt = 0, nextProgress = start + 30000;
  while (!exited && Date.now() - start < 300000) {
    if (!readyAt && fs.existsSync(path.join(proof, 'ready.json'))) {
      const ready = JSON.parse(fs.readFileSync(path.join(proof, 'ready.json'), 'utf8'));
      if (ready.pid === child.pid && ready.editorVersion === version) readyAt = Date.now();
    }
    if (Date.now() >= nextProgress) { console.log(JSON.stringify({ phase: 'waiting', ready: !!readyAt, elapsedSeconds: Math.round((Date.now() - start) / 1000) })); nextProgress += 30000; }
    if ((!readyAt && Date.now() - start > 180000) || (readyAt && Date.now() - readyAt > 90000)) break;
    await sleep(250);
  }
  let forcedCleanup = false;
  if (!exited && ownedProcesses().includes(child.pid)) {
    forcedCleanup = true; spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, encoding: 'utf8' });
    for (let i = 0; !exited && i < 40; i++) await sleep(250);
  }
  const remainingOwnedPids = ownedProcesses(), tests = [], file = path.join(proof, 'results.xml');
  if (fs.existsSync(file)) {
    const xml = fs.readFileSync(file, 'utf8');
    if (xml.length > 16 * 1024 * 1024 || /<!DOCTYPE|<!ENTITY/i.test(xml)) throw Error('Unsafe result XML.');
    const parser = require(path.join(toolkit, 'unity-cursor-toolkit/node_modules/xml2js'));
    const parsed = await parser.parseStringPromise(xml, { strict: true });
    const scrub = value => String(value || '').replaceAll(root, '<FIXTURE>').replaceAll(repo, '<FORK>').replaceAll(toolkit, '<TOOLKIT>').replaceAll(path.dirname(editor), '<EDITOR>').replaceAll(os.homedir(), '<HOME>').replace(/[A-Z]:[\\/][^\r\n<>]*/gi, '<PATH>');
    const collect = node => { for (const item of node['test-case'] || []) tests.push({ fullName: item.$.fullname, result: item.$.result, label: item.$.label || '', message: scrub(item.failure && item.failure[0].message && item.failure[0].message[0]) }); for (const suite of node['test-suite'] || []) collect(suite); };
    collect(parsed['test-run']);
  }
  const normalExitConfirmed = exited && [0, 2].includes(exitCode) && fs.existsSync(path.join(proof, 'quitting')) && !forcedCleanup && !remainingOwnedPids.length;
  const exactSelection = tests.length === methods.length && methods.every(method => tests.some(test => test.fullName === 'Unterm.Editor.Tests.RoslynCompletionTests.' + method));
  const expectedNegative = variant === 'unread-result' ? tests.every(test => test.result === 'Failed' && test.message.includes('A stopped worker published an unread result')) : methods.filter(method => method.endsWith('_Restarts') || (version === '7000.0.0a7' && method === 'ExecuteCode_CoreCLR_RefusesBeforeCompilation')).every(method => tests.some(test => test.fullName.endsWith('.' + method) && test.result === 'Failed'));
  const passed = normalExitConfirmed && exactSelection && (variant === 'fixed' ? tests.every(test => test.result === 'Passed' || (version === '6000.3.9f1' && test.fullName.endsWith('ExecuteCode_CoreCLR_RefusesBeforeCompilation') && test.result === 'Skipped')) : expectedNegative);
  const diagnosticFile = path.join(proof, 'roslyn-errors');
  const roslynDiagnostics = fs.existsSync(diagnosticFile) ? fs.readFileSync(diagnosticFile, 'utf8').split(/\r?\n/).filter(Boolean).map(line => line.replace(/[A-Z]:[\\/][^\r\n<>]*/gi, '<PATH>')) : [];
  const observation = { ...prepared, roslynDiagnostics, checkedAt: new Date().toISOString(), passed, normalExitConfirmed, exitCode, spawnError, forcedCleanup, remainingOwnedPids, exactSelection, tests };
  const out = path.join(__dirname, 'evidence', observation.checkedAt.replace(/[:.]/g, '-'));
  fs.mkdirSync(out, { recursive: true }); fs.writeFileSync(path.join(out, 'observation.json'), JSON.stringify(observation, null, 2) + '\n');
  console.log(JSON.stringify({ phase: 'complete', passed, variant, normalExitConfirmed, exitCode, tests: tests.map(test => ({ name: test.fullName.split('.').pop(), result: test.result })), evidence: path.relative(repo, path.join(out, 'observation.json')) }));
  if (passed && !ownedProcesses().length) {
    const resolved = path.resolve(root), temp = path.resolve(os.tmpdir()) + path.sep;
    if (!resolved.startsWith(temp) || !path.basename(resolved).startsWith('unterm-lifecycle-')) throw Error('Fixture cleanup boundary failed.');
    fs.rmSync(resolved, { recursive: true });
  } else console.log(JSON.stringify({ retainedFixture: root }));
  process.exitCode = passed ? 0 : 1;
}
execute().catch(error => { console.error(error.code || error.message.replaceAll(os.homedir(), '<HOME>')); process.exitCode = 1; });

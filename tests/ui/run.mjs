import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// This runner exercises a browser preview and a synthetic native bridge separately.
// It does not start the Rust application, use real identities, or touch the OS clipboard.
const root = fileURLToPath(new URL('../../', import.meta.url));
const artifacts = process.env.UI_TEST_ARTIFACTS_DIR ? resolve(process.env.UI_TEST_ARTIFACTS_DIR) : null;
let browser;
let preview;
let activePage;
let previewOutput = '';

async function freeLoopbackPort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return port;
}

async function startPreview() {
  const port = await freeLoopbackPort();
  const url = `http://127.0.0.1:${port}`;
  preview = spawn(process.execPath, [join(root, 'ui', 'preview.mjs'), '--port', String(port)], {
    cwd: root, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true,
  });
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`Preview did not start within 10 seconds.\n${previewOutput}`)), 10_000);
    const finished = (error) => { clearTimeout(timer); error ? reject(error) : resolve(); };
    preview.once('error', finished);
    preview.once('exit', (code) => finished(new Error(`Preview exited during startup (${code}).\n${previewOutput}`)));
    preview.stderr.on('data', (data) => { previewOutput = `${previewOutput}${data}`.slice(-4000); });
    preview.stdout.on('data', (data) => {
      previewOutput = `${previewOutput}${data}`.slice(-4000);
      if (previewOutput.includes(`browser preview: ${url}`)) finished();
    });
  });
  return url;
}

async function screenshot(page, name) {
  if (!artifacts) return;
  await mkdir(artifacts, { recursive: true });
  await page.screenshot({ path: join(artifacts, name), fullPage: true });
}

async function navigateToPage(page, name) {
  const link = page.getByRole('link', { name, exact: true });
  await link.click();
  // Hash navigation and its handler can finish after click() resolves. Waiting
  // for the hash alone is insufficient; assert the rendered destination instead.
  await page.locator(`#page-${name.toLowerCase()}`).waitFor({ state: 'visible', timeout: 5000 });
  assert.equal(await link.getAttribute('aria-current'), 'page', `Active navigation item for ${name}`);
}

async function runPreviewChecks(url) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 1050 }, deviceScaleFactor: 1, reducedMotion: 'reduce' });
  // Exercise delayed hashchange delivery so navigation checks cannot depend on
  // a click returning after the page has rendered. This affects this fixture only.
  await context.addInitScript(() => {
    window.addEventListener('hashchange', (event) => {
      if (!event.isTrusted) return;
      event.stopImmediatePropagation();
      setTimeout(() => window.dispatchEvent(new HashChangeEvent('hashchange', {
        oldURL: event.oldURL, newURL: event.newURL,
      })), 100);
    }, { capture: true });
  });
  const page = await context.newPage();
  activePage = page;
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await page.goto(url, { waitUntil: 'networkidle' });
  assert.equal(await page.locator('#connection-pill span').textContent(), 'Browser preview');
  assert.equal(await page.locator('#preview-banner').isVisible(), true);
  for (const checkbox of await page.getByRole('checkbox').all()) assert.equal(await checkbox.isEnabled(), false);
  assert.equal(await page.locator('#remote-empty').isVisible(), true);
  assert.equal(await page.locator('#remote-machine').isVisible(), false);
  assert.equal(await page.locator('#export-diagnostics').isEnabled(), false);
  assert.equal(await page.locator('#open-log-folder').isEnabled(), false);
  await screenshot(page, 'first-run-desktop.png');

  for (const [name, heading] of [
    ['Sharing', 'Share what you need.'], ['Security', 'Private, from the start.'],
    ['Diagnostics', 'Connection, in focus.'], ['Troubleshooting', 'Get connected.'], ['Devices', 'Your desk, connected.'],
  ]) {
    await navigateToPage(page, name);
    assert.equal(await page.getByRole('heading', { name: heading, exact: true }).isVisible(), true);
    if (name === 'Diagnostics') await screenshot(page, 'diagnostics-preview.png');
    if (name === 'Troubleshooting') {
      assert.equal(await page.locator('#add-firewall-rule').isEnabled(), false);
      assert.equal(await page.locator('#firewall-address').isEnabled(), false);
      assert.equal(await page.locator('#firewall-preview-note').isVisible(), true);
      assert.match(await page.locator('.firewall-scope').textContent(), /TCP 24859.*One peer IP.*Private \/ Domain/);
      await screenshot(page, 'troubleshooting-preview.png');
    }
  }

  const projectLink = page.getByRole('link', { name: 'Windows Preview · Open project on GitHub', exact: true });
  assert.equal(await projectLink.getAttribute('href'), 'https://github.com/bigsk1/easy-mouse-connect');
  assert.equal(await projectLink.getAttribute('target'), '_blank');
  assert.match(await projectLink.getAttribute('rel'), /noopener/);

  const pairButton = page.getByRole('button', { name: 'Pair a device', exact: true });
  assert.equal(await pairButton.count(), 1);
  await pairButton.click();
  const dialog = page.getByRole('dialog', { name: 'Pair your other device.' });
  assert.equal(await dialog.isVisible(), true);
  assert.equal(await page.getByRole('button', { name: 'Pair devices', exact: true }).isEnabled(), false);
  assert.match(await page.locator('#pair-message').textContent(), /browser preview cannot pair/);
  await page.getByRole('tab', { name: 'Show a pairing code', exact: true }).click();
  assert.equal(await page.getByRole('button', { name: 'Generate code', exact: true }).isEnabled(), false);
  assert.equal(await page.locator('#invitation-content').isVisible(), false);
  await screenshot(page, 'pair-dialog.png');
  await page.getByRole('button', { name: 'Close pairing', exact: true }).click();
  assert.equal(await dialog.isVisible(), false);
  assert.equal(await pairButton.evaluate((node) => document.activeElement === node), true, 'Pair dialog must restore focus');
  await pairButton.click();
  await page.keyboard.press('Escape');
  assert.equal(await dialog.isVisible(), false);

  for (const width of [390, 760, 1024, 1280]) {
    await page.setViewportSize({ width, height: 900 });
    for (const name of ['Devices', 'Sharing', 'Security', 'Diagnostics', 'Troubleshooting']) {
      await navigateToPage(page, name);
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
      assert.equal(overflow, false, `Horizontal overflow at ${width}px on ${name}`);
      if (name === 'Devices' && width > 650) {
        const heights = await page.locator('.overview-grid>.panel').evaluateAll((panels) => panels.map((panel) => panel.getBoundingClientRect().height));
        assert.ok(Math.abs(heights[0] - heights[1]) <= 1, `Desk controls and Preferences must have equal heights at ${width}px`);
      }
    }
    if (width === 390) {
      await navigateToPage(page, 'Devices');
      assert.equal(await page.locator('.statusbar-help').isVisible(), false);
      await screenshot(page, 'first-run-narrow.png');
    }
  }
  assert.deepEqual(errors, [], 'Unexpected browser JavaScript errors in preview');
  await context.close(); activePage = null;
  console.log('PASS browser preview: honest unavailable state, five pages, scoped setup action, fixed project link, equal control panels, modal/focus, responsive layouts (390/760/1024/1280px).');
}

function installSyntheticNativeBridge({ failStatus = false } = {}) {
  // Synthetic pairing codes and invalid public keys. No real engine or session exists.
  window.uiTest = { calls: [], listeners: {}, failStatus, status: {
    device_name: 'Synthetic UI device', public_key: 'synthetic-public-key',
    local_addresses: ['100.100.94.12', 'fd7a:115c:a1e0::1'], enabled: true, connected: false,
    remote_name: null, peer: null,
    settings: { enabled: true, share_clipboard: false, share_files: false, edge_switching: true, peer_on_left: false },
    invitation: 'AAAA-BBBB-CCCC', invitation_seconds_left: 123,
    last_error: null, latency_ms: null, pending_files: [], recent_events: ['Synthetic UI engine started'],
    receiving_directory: 'C:\\SyntheticUiFixture\\Received', native_input: true,
    diagnostics: { directory: 'C:\\SyntheticUiFixture\\Logs', max_files: 3, max_file_bytes: 2097152, retained_bytes: 1048576, dropped_events: 2, write_failures: 0 },
    input_diagnostics: { raw_input_registered: true, per_monitor_dpi: true, monitor_count: 2, virtual_width: 3840, virtual_height: 1080, raw_motion_packets: 1500, captured_moves: 1000, injected_moves: 400, injection_failures: 0, mouse_hook_installed: true, keyboard_hook_installed: true, hook_delivery_stalls: 2, hook_recoveries: 1, held_input_recovery_required: false, held_physical_keys: 0, held_physical_buttons: 0, protected_physical_keys: 0, protected_physical_buttons: 0, cursor_overlay_failures: 1, cursor_overlay_recoveries: 1 },
    control_diagnostics: { outgoing_queue_peak: 2, incoming_queue_peak: 3, outgoing_moves_coalesced: 5, incoming_moves_coalesced: 4, max_outgoing_queue_age_ms: 4, max_incoming_queue_age_ms: 5, max_native_apply_ms: 8, stale_input_failures: 0, input_queue_overflows: 0, round_trip_sample_age_ms: 100 },
  } };
  window.__TAURI__ = {
    event: { listen: async (name, callback) => { uiTest.listeners[name] = callback; return () => {}; } },
    core: { invoke: async (command, args) => {
      uiTest.calls.push({ command, args });
      if (command === 'get_status') {
        if (uiTest.failStatus) throw 'Synthetic engine unavailable';
        return structuredClone(uiTest.status);
      }
      if (command === 'get_diagnostic_status') return structuredClone(uiTest.status.diagnostics);
      if (command === 'update_settings') {
        uiTest.status.settings = args.settings; uiTest.status.enabled = args.settings.enabled;
        return structuredClone(uiTest.status);
      }
      if (command === 'create_invitation') {
        if (uiTest.deferInvitation) return new Promise((resolve) => {
          uiTest.finishInvitation = () => {
            uiTest.status.invitation = 'DDDD-EEEE-FFFF';
            uiTest.deferInvitation = false;
            resolve(uiTest.status.invitation);
          };
        });
        uiTest.status.invitation = 'ABCD-EFGH-JKMN';
        return uiTest.status.invitation;
      }
      if (command === 'pair_device') {
        uiTest.status.peer = { name: '<img src=x onerror=alert(1)>', address: args.address, public_key: 'synthetic-peer-public-key' };
        uiTest.status.connected = true; uiTest.status.latency_ms = 9;
      }
      if (command === 'respond_file') uiTest.status.pending_files = uiTest.status.pending_files.filter((file) => file.id !== args.id);
      if (command === 'diagnostics') return 'Synthetic diagnostic <b>literal text</b>';
      if (command === 'add_peer_firewall_rule') {
        if (uiTest.firewallError) throw uiTest.firewallError;
        if (uiTest.deferFirewall) return new Promise((resolve) => {
          uiTest.finishFirewall = () => {
            uiTest.deferFirewall = false;
            resolve('Synthetic firewall rule <b>added</b>');
          };
        });
        return 'Synthetic firewall rule added.';
      }
      if (command === 'export_diagnostics') return 'C:\\SyntheticUiFixture\\Exports\\diagnostics.zip';
      if (command === 'emergency_stop') {
        uiTest.status.settings.enabled = false; uiTest.status.enabled = false; uiTest.status.connected = false;
      }
      if (command === 'forget_peer') { uiTest.status.peer = null; uiTest.status.connected = false; }
    } },
  };
}

async function runSyntheticNativeChecks(url) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 1050 }, reducedMotion: 'reduce' });
  await context.addInitScript(installSyntheticNativeBridge);
  const page = await context.newPage(); activePage = page;
  const errors = []; page.on('pageerror', (error) => errors.push(error.message));
  await page.goto(url, { waitUntil: 'networkidle' });
  assert.equal(await page.locator('#connection-pill span').textContent(), 'Ready to pair');
  assert.equal(await page.locator('#preview-banner').isVisible(), false);
  assert.equal(await page.getByRole('checkbox', { name: 'Enable desk sharing', exact: true }).isChecked(), true);
  // Older stored settings omit these fields; the UI must never infer permission.
  for (const name of ['Keep the other PC awake', 'Show clipboard and network notifications']) {
    assert.equal(await page.getByRole('checkbox', { name, exact: true }).isChecked(), false);
  }
  await page.getByRole('checkbox', { name: 'Keep the other PC awake', exact: true }).check();
  await page.waitForFunction(() => uiTest.status.settings.keep_peer_awake);
  await page.getByRole('checkbox', { name: 'Show clipboard and network notifications', exact: true }).check();
  await page.waitForFunction(() => uiTest.status.settings.show_status_notifications);
  assert.match(await page.locator('#keep-awake-scope').textContent(), /screen saver and lock settings stay in effect/);
  await page.getByRole('link', { name: 'Windows Preview · Open project on GitHub', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'open_project_url')), true);
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'open_project_url').args), undefined, 'Project opener must not accept a frontend URL');

  await page.getByRole('button', { name: 'Pair a device', exact: true }).click();
  await page.getByRole('tab', { name: 'Show a pairing code', exact: true }).click();
  assert.equal(await page.locator('#invitation-content').isVisible(), false, 'Do not reveal a prior invitation without explicit generation');
  await page.getByRole('button', { name: 'Generate code', exact: true }).click();
  await page.locator('#invitation-content').waitFor({ state: 'visible' });
  assert.equal(await page.locator('#invitation-code').inputValue(), 'ABCD-EFGH-JKMN');
  await page.getByRole('button', { name: 'Copy code', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'copy_invitation')), true);
  await page.getByRole('button', { name: 'Close pairing', exact: true }).click();
  await page.getByRole('button', { name: 'Pair a device', exact: true }).click();
  await page.getByRole('tab', { name: 'Show a pairing code', exact: true }).click();
  assert.equal(await page.locator('#invitation-content').isVisible(), false, 'Closing the modal must clear previously revealed invitations');
  await page.evaluate(() => { uiTest.deferInvitation = true; });
  await page.getByRole('button', { name: 'Generate code', exact: true }).click();
  await page.waitForFunction(() => typeof uiTest.finishInvitation === 'function');
  await page.getByRole('button', { name: 'Close pairing', exact: true }).click();
  await page.evaluate(() => uiTest.finishInvitation());
  await page.waitForFunction(() => !document.querySelector('#create-invitation').disabled);
  await page.getByRole('button', { name: 'Pair a device', exact: true }).click();
  await page.getByRole('tab', { name: 'Show a pairing code', exact: true }).click();
  assert.equal(await page.locator('#invitation-content').isVisible(), false, 'A late invitation result must not reveal a secret after closing/reopening');
  await page.getByRole('tab', { name: 'Connect to a device', exact: true }).click();
  await page.getByLabel("Other device's IP address", { exact: true }).fill('100.71.9.14');
  await page.getByLabel("Other device's pairing code", { exact: true }).fill('mnpqrstvwxyz');
  assert.equal(await page.locator('#peer-invitation').inputValue(), 'MNPQ-RSTV-WXYZ', 'Typed code must be grouped and capitalized');
  const callsBefore = await page.evaluate(() => uiTest.calls.filter((call) => call.command === 'get_status').length);
  await page.waitForFunction((count) => uiTest.calls.filter((call) => call.command === 'get_status').length > count, callsBefore);
  assert.equal(await page.locator('#peer-address').inputValue(), '100.71.9.14', 'Polling must preserve the typed address');
  assert.equal(await page.locator('#peer-invitation').inputValue(), 'MNPQ-RSTV-WXYZ', 'Polling must preserve the typed invitation');
  await page.getByRole('button', { name: 'Pair devices', exact: true }).click();
  await page.locator('#pair-dialog').waitFor({ state: 'hidden' });
  assert.deepEqual(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'pair_device').args), { address: '100.71.9.14', invitation: 'MNPQ-RSTV-WXYZ' });
  assert.equal(await page.locator('#connection-pill span').textContent(), 'Connected');
  assert.equal(await page.locator('#remote-name').textContent(), '<img src=x onerror=alert(1)>');
  assert.equal(await page.locator('#remote-name img').count(), 0, 'Untrusted peer names must remain text');
  await page.getByRole('button', { name: 'Peer on left', exact: true }).click();
  await page.waitForFunction(() => uiTest.status.settings.peer_on_left);
  assert.equal(await page.getByRole('button', { name: 'Peer on left', exact: true }).getAttribute('aria-pressed'), 'true');

  await navigateToPage(page, 'Troubleshooting');
  assert.equal(await page.locator('#firewall-address').inputValue(), '100.71.9.14', 'A paired endpoint should prefill the setup field');
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'add_peer_firewall_rule')), false, 'Firewall setup requires an explicit action');
  await page.locator('#firewall-address').fill(' [2001:db8::20]:24859 ');
  const firewallStatusCalls = await page.evaluate(() => uiTest.calls.filter((call) => call.command === 'get_status').length);
  await page.waitForFunction((count) => uiTest.calls.filter((call) => call.command === 'get_status').length > count, firewallStatusCalls);
  assert.equal(await page.locator('#firewall-address').inputValue(), ' [2001:db8::20]:24859 ', 'Polling must preserve the user-edited firewall endpoint');
  await page.evaluate(() => { uiTest.deferFirewall = true; });
  await page.getByRole('button', { name: 'Add a firewall rule', exact: true }).click();
  await page.waitForFunction(() => typeof uiTest.finishFirewall === 'function');
  assert.equal(await page.locator('#add-firewall-rule').isEnabled(), false);
  assert.equal(await page.locator('#firewall-address').isEnabled(), false);
  assert.match(await page.locator('#firewall-result').textContent(), /Waiting for Windows administrator approval/);
  await page.evaluate(() => document.querySelector('#firewall-form').requestSubmit());
  assert.equal(await page.evaluate(() => uiTest.calls.filter((call) => call.command === 'add_peer_firewall_rule').length), 1, 'A pending UAC action must not start a second helper');
  assert.deepEqual(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'add_peer_firewall_rule').args), { address: '[2001:db8::20]:24859' }, 'The native helper only receives a trimmed address');
  await page.evaluate(() => uiTest.finishFirewall());
  await page.waitForFunction(() => !document.querySelector('#add-firewall-rule').disabled);
  assert.equal(await page.locator('#firewall-result').textContent(), 'Synthetic firewall rule <b>added</b>');
  assert.equal(await page.locator('#firewall-result b').count(), 0);
  await page.evaluate(() => { uiTest.firewallError = 'Windows approval was cancelled <b>literal</b>'; });
  await page.getByRole('button', { name: 'Add a firewall rule', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#firewall-result').classList.contains('error'));
  assert.equal(await page.locator('#firewall-result').textContent(), 'Windows approval was cancelled <b>literal</b>');
  assert.equal(await page.locator('#firewall-result b').count(), 0);
  assert.equal(await page.getByRole('button', { name: 'Add a firewall rule', exact: true }).isEnabled(), true);
  await screenshot(page, 'troubleshooting-native.png');

  await navigateToPage(page, 'Sharing');
  assert.equal(await page.getByRole('checkbox', { name: 'Share plain-text clipboard', exact: true }).isChecked(), false);
  assert.equal(await page.getByRole('checkbox', { name: 'Allow file transfers', exact: true }).isChecked(), false);
  assert.equal(await page.getByRole('checkbox', { name: 'Share image clipboard', exact: true }).isChecked(), false);
  await page.getByRole('checkbox', { name: 'Share image clipboard', exact: true }).check();
  await page.waitForFunction(() => uiTest.status.settings.share_clipboard_images);
  assert.equal(await page.getByRole('checkbox', { name: 'Share plain-text clipboard', exact: true }).isChecked(), false, 'Image permission must remain independent of plain text');
  assert.equal(await page.getByRole('checkbox', { name: 'Allow file transfers', exact: true }).isChecked(), false, 'Image permission must remain independent of files');
  await page.getByRole('checkbox', { name: 'Share plain-text clipboard', exact: true }).check();
  await page.waitForFunction(() => uiTest.status.settings.share_clipboard);
  await page.getByRole('checkbox', { name: 'Allow file transfers', exact: true }).check();
  await page.waitForFunction(() => uiTest.status.settings.share_files);
  await page.evaluate(() => uiTest.listeners['tauri://drag-drop']({ payload: { paths: ['C:\\SyntheticUiFixture\\<example>.txt'] } }));
  assert.equal(await page.locator('#selected-file-name').textContent(), '<example>.txt');
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'send_file')), false, 'Drop alone must not send a file');
  await page.getByRole('button', { name: 'Send file', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'send_file')?.args.path), 'C:\\SyntheticUiFixture\\<example>.txt');
  await page.evaluate(() => { uiTest.status.pending_files = [{ id: 'synthetic-file-id', name: '<img src=x onerror=alert(1)>.txt', size: 1200 }]; });
  await page.getByRole('button', { name: 'Accept file', exact: true }).waitFor({ state: 'visible' });
  assert.equal(await page.locator('#file-inbox img').count(), 0, 'Untrusted filenames must remain text');
  await page.getByRole('button', { name: 'Accept file', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'respond_file' && call.args.accept === true && call.args.id === 'synthetic-file-id')), true);

  await navigateToPage(page, 'Diagnostics');
  assert.equal(await page.locator('#logs-directory').textContent(), 'C:\\SyntheticUiFixture\\Logs');
  assert.equal(await page.locator('#logs-retained').textContent(), '1.00 MiB retained · 6 MiB limit');
  assert.equal(await page.locator('#logs-health').textContent(), '2 dropped events · 0 write failures');
  assert.equal(await page.locator('#input-mode').textContent(), 'Local control');
  assert.equal(await page.locator('#metric-input').textContent(), 'Local control');
  assert.equal(await page.locator('#input-recovery-hint').isVisible(), false);
  assert.match(await page.locator('#metric-latency-note').textContent(), /Network only/);
  assert.match(await page.locator('#control-counters').textContent(), /Longest native apply8 ms/);
  assert.match(await page.locator('#input-facts').textContent(), /2 monitors/);
  assert.match(await page.locator('#input-facts').textContent(), /mouse yes, keyboard yes \(installation result only\)/);
  assert.match(await page.locator('#input-counters').textContent(), /Suspected hook delivery stalls2/);
  assert.match(await page.locator('#input-counters').textContent(), /Raw ownership query failures0/);
  assert.match(await page.locator('#input-counters').textContent(), /Raw ownership mismatches0/);
  assert.match(await page.locator('#input-counters').textContent(), /Source edge checks0/);
  await page.evaluate(() => {
    Object.assign(uiTest.status.control_diagnostics, {
      outgoing_queue_stale_failures: 2, outgoing_write_deadline_failures: 3,
      incoming_queue_stale_failures: 4, native_completion_deadline_failures: 5,
    });
    Object.assign(uiTest.status.input_diagnostics, {
      native_commands: 19, native_queue_wait_peak_us: 2100, native_execution_peak_us: 3200,
      source_edge_samples: 150, source_edge_candidates: 40, source_edge_intent_ready: 7,
      source_enter_committed: 6, remote_enter_committed: 3, remote_edge_returns: 2,
      captured_wheels: 83, injected_wheels: 47, native_sendinput_calls: 290,
      native_sendinput_total_us: 9000, native_sendinput_peak_us: 4000, native_wheel_peak_us: 3800,
    });
  });
  await page.waitForFunction(() => document.getElementById('control-counters').textContent.includes('Native completion timed out5'));
  assert.match(await page.locator('#control-counters').textContent(), /Source queue expired2/);
  assert.match(await page.locator('#control-counters').textContent(), /Send deadline exceeded3/);
  assert.match(await page.locator('#control-counters').textContent(), /Receive queue expired4/);
  assert.match(await page.locator('#input-counters').textContent(), /Native commands completed19/);
  assert.match(await page.locator('#input-counters').textContent(), /Longest native queue wait \(µs\)2,100/);
  assert.match(await page.locator('#input-counters').textContent(), /Longest native execution \(µs\)3,200/);
  assert.match(await page.locator('#input-counters').textContent(), /Source edge checks150/);
  assert.match(await page.locator('#input-counters').textContent(), /Eligible edge samples40/);
  assert.match(await page.locator('#input-counters').textContent(), /Outward gestures ready7/);
  assert.match(await page.locator('#input-counters').textContent(), /Source entries committed6/);
  assert.match(await page.locator('#input-counters').textContent(), /Remote entries committed3/);
  assert.match(await page.locator('#input-counters').textContent(), /Returns through edge2/);
  assert.match(await page.locator('#input-counters').textContent(), /Forwarded wheel events83/);
  assert.match(await page.locator('#input-counters').textContent(), /Injected wheel events47/);
  assert.match(await page.locator('#input-counters').textContent(), /Windows input calls290/);
  assert.match(await page.locator('#input-counters').textContent(), /Total Windows input time \(µs\)9,000/);
  assert.match(await page.locator('#input-counters').textContent(), /Longest Windows input call \(µs\)4,000/);
  assert.match(await page.locator('#input-counters').textContent(), /Longest wheel input call \(µs\)3,800/);
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'export_diagnostics')), false, 'Diagnostics export must require a click');
  await page.getByRole('button', { name: 'Export diagnostic ZIP', exact: true }).click();
  await page.locator('#export-result').waitFor({ state: 'visible' });
  assert.equal(await page.locator('#export-path').textContent(), 'C:\\SyntheticUiFixture\\Exports\\diagnostics.zip');
  await page.getByRole('button', { name: 'Open export folder', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'open_export_folder')), true);
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'open_export_folder')?.args), undefined, 'Export folder opener accepts no frontend path');
  await page.getByRole('button', { name: 'Open logs folder', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'open_log_folder')?.args), undefined, 'Folder opener must not accept a UI-supplied path');
  await page.getByLabel('Device IP address', { exact: true }).fill('fd7a:115c:a1e0::1');
  await page.getByRole('button', { name: 'Run check', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'diagnostics')?.args.address), 'fd7a:115c:a1e0::1');
  assert.equal(await page.locator('#diagnostics-output').textContent(), 'Synthetic diagnostic <b>literal text</b>');
  assert.equal(await page.locator('#diagnostics-output b').count(), 0);
  await page.evaluate(() => { uiTest.status.control_diagnostics.round_trip_sample_age_ms = 6500; uiTest.status.input_diagnostics.sending = true; });
  await page.waitForFunction(() => document.getElementById('metric-input').textContent === 'Sending');
  assert.match(await page.locator('#metric-latency').textContent(), /—/);
  assert.match(await page.locator('#metric-latency-note').textContent(), /sample is stale/);
  assert.match(await page.locator('#metric-input-note').textContent(), /go to the paired PC/);
  await page.evaluate(() => { uiTest.status.control_diagnostics.round_trip_sample_age_ms = 100; uiTest.status.input_diagnostics.sending = false; uiTest.status.input_diagnostics.receiving = true; });
  await page.waitForFunction(() => document.getElementById('metric-input').textContent === 'Receiving');
  assert.match(await page.locator('#metric-latency').textContent(), /9/);
  await page.evaluate(() => {
    uiTest.status.input_diagnostics.receiving = false;
    uiTest.status.input_diagnostics.held_input_recovery_required = true;
    uiTest.status.input_diagnostics.held_physical_keys = 1;
  });
  await page.locator('#input-recovery-hint').waitFor({ state: 'visible' });
  assert.match(await page.locator('#input-recovery-hint').textContent(), /press and release/);
  await navigateToPage(page, 'Devices');
  assert.equal(await page.locator('#input-recovery-hint').isVisible(), true, 'Local release guidance must remain visible outside Diagnostics');
  await page.evaluate(() => {
    uiTest.status.input_diagnostics.held_input_recovery_required = false;
    uiTest.status.input_diagnostics.held_physical_keys = 0;
  });
  await page.locator('#input-recovery-hint').waitFor({ state: 'hidden' });
  await navigateToPage(page, 'Security');
  await page.evaluate(() => { uiTest.status.input_diagnostics.held_input_recovery_required = true; });
  await page.locator('#input-recovery-hint').waitFor({ state: 'visible' });
  await page.getByRole('button', { name: 'Stop sharing now', exact: true }).click();
  await page.waitForFunction(() => uiTest.status.settings.enabled === false);
  await page.locator('#input-recovery-hint').waitFor({ state: 'hidden' });
  assert.equal(await page.locator('#connection-pill span').textContent(), 'Sharing paused');
  await page.evaluate(() => { uiTest.failStatus = true; });
  await page.locator('#connection-pill').getByText('Engine unavailable', { exact: true }).waitFor();
  assert.equal(await page.getByRole('button', { name: 'Stop sharing now', exact: true }).isEnabled(), false);
  await page.evaluate(() => { uiTest.failStatus = false; });
  await page.locator('#connection-pill').getByText('Sharing paused', { exact: true }).waitFor();
  await navigateToPage(page, 'Devices');
  assert.equal(await page.getByRole('checkbox', { name: 'Enable desk sharing', exact: true }).isEnabled(), true);
  assert.deepEqual(errors, [], 'Unexpected browser JavaScript errors with the synthetic bridge');
  await context.close(); activePage = null;
  console.log('PASS synthetic native bridge: typed pairing code/copy/privacy, pairing, polling, legacy defaults/new settings, fixed project opener, scoped firewall action/prefill/UAC wait/error, safe text, explicit send/accept, local diagnostics/export, IPv6 diagnostics, emergency stop, engine failure/recovery.');
}

async function runStartupFailureChecks(url) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 1050 }, reducedMotion: 'reduce' });
  await context.addInitScript(installSyntheticNativeBridge, { failStatus: true });
  const page = await context.newPage(); activePage = page;
  const errors = []; page.on('pageerror', (error) => errors.push(error.message));
  await page.goto(url, { waitUntil: 'networkidle' });
  await navigateToPage(page, 'Diagnostics');
  assert.equal(await page.locator('#connection-pill span').textContent(), 'Engine unavailable');
  assert.match(await page.locator('#startup-error').textContent(), /Synthetic engine unavailable/);
  assert.equal(await page.locator('#metric-input').textContent(), 'Unavailable');
  assert.equal(await page.locator('#input-mode').textContent(), 'Unavailable');
  assert.equal(await page.locator('#logs-directory').textContent(), 'C:\\SyntheticUiFixture\\Logs');
  assert.equal(await page.getByRole('button', { name: 'Run check', exact: true }).isEnabled(), false);
  assert.equal(await page.getByRole('button', { name: 'Export diagnostic ZIP', exact: true }).isEnabled(), true);
  await page.getByRole('button', { name: 'Export diagnostic ZIP', exact: true }).click();
  await page.locator('#export-result').waitFor({ state: 'visible' });
  assert.equal(await page.locator('#export-path').textContent(), 'C:\\SyntheticUiFixture\\Exports\\diagnostics.zip');
  await page.getByRole('button', { name: 'Open export folder', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'open_export_folder')), true);
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'open_export_folder')?.args), undefined, 'Export folder opener accepts no frontend path');
  await page.getByRole('button', { name: 'Open logs folder', exact: true }).click();
  assert.equal(await page.evaluate(() => uiTest.calls.some((call) => call.command === 'open_log_folder')), true);
  assert.equal(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'open_log_folder').args), undefined);
  await navigateToPage(page, 'Devices');
  assert.equal(await page.getByRole('checkbox', { name: 'Enable desk sharing', exact: true }).isEnabled(), false);
  assert.equal(await page.locator('#remote-machine').isVisible(), false);
  await navigateToPage(page, 'Troubleshooting');
  assert.equal(await page.locator('#firewall-address').inputValue(), '', 'An unpaired setup must not invent a peer address');
  assert.equal(await page.locator('#add-firewall-rule').isEnabled(), false);
  assert.equal(await page.locator('#firewall-address').isEnabled(), true, 'Connection setup remains usable when the engine is unavailable');
  await page.locator('#firewall-address').fill('192.0.2.20');
  await page.getByRole('button', { name: 'Add a firewall rule', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#firewall-result').textContent === 'Synthetic firewall rule added.');
  assert.deepEqual(await page.evaluate(() => uiTest.calls.find((call) => call.command === 'add_peer_firewall_rule').args), { address: '192.0.2.20' });
  assert.deepEqual(errors, []);
  await context.close(); activePage = null;
  console.log('PASS synthetic startup failure: actual error shown, logs/export/folder and unpaired firewall setup remain available, connection/input controls stay unavailable.');
}

async function cleanup() {
  if (browser) await browser.close().catch(() => {});
  if (preview && preview.exitCode === null && preview.signalCode === null) {
    const stopped = new Promise((resolve) => preview.once('exit', resolve));
    preview.kill();
    const timer = setTimeout(() => preview.kill('SIGKILL'), 3000);
    await stopped; clearTimeout(timer);
  }
}

try {
  const { chromium } = await import('playwright');
  const url = await startPreview();
  browser = await chromium.launch({
    headless: true,
    ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH } : {}),
  });
  await runPreviewChecks(url);
  await runSyntheticNativeChecks(url);
  await runStartupFailureChecks(url);
  console.log('These checks do not validate Rust/Tauri IPC, encryption, Windows input/clipboard, or two-PC behavior.');
  if (artifacts) console.log(`Screenshots: ${artifacts}`);
} catch (error) {
  console.error('FAIL UI regression:', error);
  if (activePage && !activePage.isClosed()) {
    const failureDirectory = artifacts || await mkdtemp(join(tmpdir(), 'emc-ui-failure-'));
    await mkdir(failureDirectory, { recursive: true });
    const path = join(failureDirectory, 'failure.png');
    await activePage.screenshot({ path, fullPage: true }).then(() => console.error(`Failure screenshot: ${path}`)).catch(() => {});
  }
  process.exitCode = 1;
} finally { await cleanup(); }

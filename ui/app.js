/* No web dependencies. Native work is delegated to the local Rust engine. */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const native = typeof window.__TAURI__?.core?.invoke === 'function';
  const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
  const defaults = { enabled: true, share_clipboard: false, share_clipboard_images: false, share_files: false, keep_peer_awake: false, show_status_notifications: false, edge_switching: true, peer_on_left: false };
  let status = {
    device_name: 'This Windows device', public_key: '', local_addresses: [], enabled: true,
    connected: false, remote_name: null, peer: null, settings: { ...defaults },
    invitation: null, invitation_seconds_left: 0, pairing_attempts_remaining: null, last_error: null, latency_ms: null,
    pending_files: [], recent_events: [], receiving_directory: '', native_input: false,
    diagnostics: null, input_diagnostics: null, control_diagnostics: null,
  };
  let ready = false;
  let diagnosticsReady = false;
  let engineError = '';
  let settingsBusy = false;
  let selectedPath = '';
  let selectedName = '';
  let transferBusy = false;
  let pairingBusy = false;
  let invitationBusy = false;
  let diagnosticsBusy = false;
  let exportBusy = false;
  let folderBusy = false;
  let exportFolderBusy = false;
  let firewallBusy = false;
  let firewallAddressEdited = false;
  let projectOpening = false;
  let invitationRevealed = false;
  let generatedInvitation = '';
  let pairSession = 0;
  let previousFocus = null;
  let toastTimer;
  let refreshing = false;
  let eventsSignature = '';
  let offersSignature = '';

  const connected = () => native && ready && status.connected && status.settings.enabled;
  const canTransfer = () => connected() && status.settings.share_files;
  const errorText = (error) => typeof error === 'string' ? error : error?.message || 'The desktop engine could not complete this action.';
  const setText = (id, value) => {
    const node = $(id);
    const next = String(value ?? '');
    if (node.textContent !== next) node.textContent = next;
  };

  function toast(message, error = false) {
    clearTimeout(toastTimer);
    const node = $('toast');
    node.textContent = message;
    node.classList.toggle('error', error);
    node.hidden = false;
    toastTimer = setTimeout(() => { node.hidden = true; }, error ? 6500 : 3500);
  }

  function normalizeStatus(raw) {
    if (!raw || typeof raw !== 'object') throw new Error('The desktop engine returned an invalid status.');
    return {
      ...status, ...raw,
      settings: { ...defaults, ...(raw.settings || {}) },
      local_addresses: Array.isArray(raw.local_addresses) ? raw.local_addresses : [],
      pending_files: Array.isArray(raw.pending_files) ? raw.pending_files : [],
      recent_events: Array.isArray(raw.recent_events) ? raw.recent_events : [],
    };
  }

  function render() {
    const live = connected();
    const peer = status.peer;
    const controlsDisabled = !native || !ready || settingsBusy;
    let label = 'Ready to pair';
    if (!native) label = 'Browser preview';
    else if (!ready) label = 'Engine unavailable';
    else if (!status.settings.enabled) label = 'Sharing paused';
    else if (live) label = 'Connected';
    else if (peer) label = 'Waiting for peer';

    $('preview-banner').hidden = native;
    setText('engine-label', native ? (ready ? 'LOCAL ENGINE' : 'ENGINE OFFLINE') : 'DESIGN PREVIEW');
    const pill = $('connection-pill');
    pill.querySelector('span').textContent = label;
    pill.classList.toggle('connected', live);
    pill.classList.toggle('error', native && !ready);
    setText('local-name', native && ready ? status.device_name : 'This Windows device');
    setText('local-address', status.local_addresses.length ? status.local_addresses.join(' · ') : (native ? 'No local addresses reported' : 'Desktop app required'));
    setText('device-count', peer ? '2' : '1');
    $('remote-empty').hidden = Boolean(peer);
    $('remote-machine').hidden = !peer;
    $('peer-actions').hidden = !peer;
    if (peer) {
      setText('remote-name', status.remote_name || peer.name || 'Paired device');
      setText('remote-address', peer.address || '');
      setText('remote-screen-tag', live ? 'CONNECTED DEVICE' : 'PAIRED DEVICE');
      setText('peer-connection-detail', live ? `Connected to ${status.remote_name || peer.name || 'your paired device'}.` : 'Your trusted device is paired. Waiting for a connection.');
    }
    $('desk-scene').classList.toggle('peer-left', Boolean(status.settings.peer_on_left));
    $('desk-bridge').classList.toggle('connected', live);
    setText('bridge-caption', live ? 'Encrypted connection' : peer ? 'Waiting for peer' : 'Make the connection');
    setText('arrangement-description', peer ? `Your paired device sits to the ${status.settings.peer_on_left ? 'left' : 'right'} of this device.` : 'Arrange your devices to match your desk.');
    document.querySelectorAll('[data-setting]').forEach((input) => {
      input.checked = Boolean(status.settings[input.dataset.setting]);
      input.disabled = controlsDisabled;
    });
    document.querySelectorAll('[data-peer-left]').forEach((button) => {
      const selected = (button.dataset.peerLeft === 'true') === Boolean(status.settings.peer_on_left);
      button.classList.toggle('selected', selected);
      button.setAttribute('aria-pressed', String(selected));
      button.disabled = controlsDisabled;
    });
    $('reconnect-button').disabled = !ready;
    $('forget-button').disabled = !ready;
    $('emergency-stop').disabled = !ready || !status.settings.enabled;
    $('refresh-status').disabled = !native || refreshing;
    $('run-diagnostics').disabled = !native || !ready || diagnosticsBusy;
    $('pair-submit').disabled = !native || !ready || pairingBusy;
    $('create-invitation').disabled = !native || !ready || invitationBusy;
    $('copy-invitation').disabled = !native || !ready;
    setText('local-public-key', status.public_key || 'Launch the desktop app to view this device\'s public key.');
    setText('trusted-device-description', peer ? `${peer.name || 'Paired device'} · ${peer.address || 'Address unavailable'}` : 'No trusted device yet. Pair a device to establish trust.');
    $('peer-public-key').hidden = !peer;
    setText('peer-public-key', peer?.public_key || '');
    setText('metric-connection', label);
    setText('metric-connection-note', live ? 'Encrypted direct connection' : peer ? 'Trusted device saved locally' : native && ready ? 'No trusted device yet' : 'Open the desktop app');
    const latency = $('metric-latency');
    const sampleAge = status.control_diagnostics?.round_trip_sample_age_ms;
    const staleSample = sampleAge != null && Number(sampleAge) > 5000;
    latency.replaceChildren(document.createTextNode(live && status.latency_ms != null && !staleSample ? String(status.latency_ms) : '—'));
    const latencyUnit = document.createElement('small');
    latencyUnit.textContent = ' ms';
    latency.append(latencyUnit);
    setText('metric-latency-note', !live || status.latency_ms == null ? 'Waiting for a peer reply' : staleSample ? `Last reply ${Math.floor(Number(sampleAge) / 1000)}s ago · sample is stale` : sampleAge == null ? 'Network timing only · sample age unavailable' : `Network only · reply ${(Number(sampleAge) / 1000).toFixed(1)}s ago`);
    const routing = ready && status.native_input ? status.input_diagnostics : null;
    $('input-recovery-hint').hidden = !(status.settings.enabled && routing?.held_input_recovery_required && !routing.sending && !routing.receiving);
    setText('metric-input', !routing ? 'Unavailable' : routing.sending ? 'Sending' : routing.receiving ? 'Receiving' : 'Local control');
    setText('metric-input-note', !routing ? 'Windows desktop input required' : routing.sending ? 'Mouse and keyboard go to the paired PC' : routing.receiving ? 'Input arrives from the paired PC' : 'Mouse and keyboard stay on this PC');
    setText('receiving-directory', status.receiving_directory || 'Available in the desktop app');
    $('last-error').hidden = !status.last_error || Boolean(engineError);
    setText('last-error', status.last_error || '');
    setText('startup-error', engineError ? `Desktop engine unavailable: ${engineError}` : '');
    $('startup-error').hidden = !engineError;
    let footer = 'Browser preview · Desktop engine unavailable';
    if (native && !ready) footer = 'Desktop engine unavailable · Check Diagnostics';
    else if (native && live) footer = `Connected · ${status.remote_name || peer?.name || 'Paired device'}`;
    else if (native && !status.settings.enabled) footer = 'Sharing paused · This device keeps local control';
    else if (native && peer) footer = 'Trusted device saved · Waiting for connection';
    else if (native) footer = 'Ready to pair · No trusted device';
    setText('footer-status', footer);
    $('footer-dot').style.background = live ? 'var(--mint)' : '';
    renderInvitation();
    renderTransfer();
    renderOffers();
    renderEvents();
    renderDiagnostics();
    renderTroubleshooting();
  }

  function renderInvitation() {
    const addresses = $('invite-addresses');
    addresses.replaceChildren();
    if (!native || !status.local_addresses.length) addresses.textContent = 'Device addresses appear in the desktop app.';
    else status.local_addresses.forEach((address) => {
      const item = document.createElement('span');
      item.textContent = address;
      addresses.append(item);
    });
    const code = invitationRevealed ? (status.invitation || generatedInvitation) : '';
    const seconds = Number(status.invitation_seconds_left) || 0;
    const expired = invitationRevealed && !status.invitation && !generatedInvitation;
    $('invitation-content').hidden = !code;
    if ($('invitation-code').value !== code) $('invitation-code').value = code;
    const attempts = status.pairing_attempts_remaining;
    setText('invitation-expiry', seconds > 0 ? `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')} left${attempts == null ? '' : ` · ${Number(attempts)} attempts left`}` : '');
    const createLabel = invitationBusy ? 'Generating code…' : code ? 'Generate a new code' : expired ? 'Generate another code' : 'Generate code';
    $('create-invitation').replaceChildren(document.createTextNode(createLabel));
  }

  function renderTransfer() {
    const allowed = canTransfer();
    $('drop-zone').classList.toggle('ready', allowed);
    setText('drop-title', selectedPath ? 'Ready for a handoff' : 'Drop a file here');
    setText('drop-description', !native ? 'File transfer is available in the desktop app.' : !status.settings.share_files ? 'Enable file transfers and connect a device to begin.' : !connected() ? 'Connect your paired device to send a file.' : 'Your other device will ask before receiving it.');
    $('selected-file').hidden = !selectedPath;
    setText('selected-file-name', selectedName);
    $('send-file').disabled = !allowed || !selectedPath || transferBusy;
    $('send-file').replaceChildren(document.createTextNode(transferBusy ? 'Sending offer…' : 'Send file'));
    $('clear-file').disabled = transferBusy;
  }

  function formatBytes(size) {
    const bytes = Number(size);
    if (!Number.isFinite(bytes) || bytes < 0) return 'Unknown size';
    if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(1)} MB`;
    if (bytes >= 1000) return `${(bytes / 1000).toFixed(1)} kB`;
    return `${bytes} bytes`;
  }

  function renderOffers() {
    const signature = JSON.stringify([status.pending_files, ready]);
    if (signature === offersSignature) return;
    offersSignature = signature;
    const inbox = $('file-inbox');
    inbox.replaceChildren();
    if (!status.pending_files.length) {
      const empty = document.createElement('div');
      empty.className = 'inbox-empty';
      const title = document.createElement('p'); title.textContent = 'No incoming files';
      const subtitle = document.createElement('span'); subtitle.textContent = 'File requests appear here.';
      empty.append(title, subtitle); inbox.append(empty); return;
    }
    status.pending_files.forEach((file) => {
      const row = document.createElement('article'); row.className = 'file-offer';
      const name = document.createElement('strong'); name.textContent = file.name;
      const detail = document.createElement('p'); detail.textContent = `${formatBytes(file.size)} · From your trusted device`;
      const actions = document.createElement('div'); actions.className = 'offer-actions';
      for (const [accept, label] of [[true, 'Accept file'], [false, 'Decline']]) {
        const button = document.createElement('button'); button.type = 'button';
        button.className = `button small ${accept ? 'primary' : 'subtle'}`;
        button.textContent = label; button.disabled = !ready;
        button.addEventListener('click', async () => {
          actions.querySelectorAll('button').forEach((item) => { item.disabled = true; });
          try { await invoke('respond_file', { id: file.id, accept }); await refresh(); toast(accept ? 'File accepted. The device will start the transfer.' : 'File request declined.'); }
          catch (error) { toast(errorText(error), true); actions.querySelectorAll('button').forEach((item) => { item.disabled = !ready; }); }
        });
        actions.append(button);
      }
      row.append(name, detail, actions); inbox.append(row);
    });
  }

  function renderEvents() {
    const signature = JSON.stringify(status.recent_events);
    if (eventsSignature === signature) return;
    eventsSignature = signature;
    const list = $('recent-events'); list.replaceChildren();
    for (const event of status.recent_events.length ? status.recent_events : ['No engine events available.']) {
      const item = document.createElement('li'); item.textContent = event; list.append(item);
    }
  }

  function renderDiagnostics() {
    const logs = diagnosticsReady ? status.diagnostics : null;
    setText('logs-directory', logs?.directory || 'Available in the desktop app');
    setText('logs-retained', logs ? `${(Number(logs.retained_bytes || 0) / 1048576).toFixed(2)} MiB retained · ${(Number(logs.max_files) * Number(logs.max_file_bytes) / 1048576).toFixed(0)} MiB limit` : 'Retention unavailable');
    setText('logs-health', logs ? `${Number(logs.dropped_events || 0)} dropped events · ${Number(logs.write_failures || 0)} write failures` : 'Desktop engine required');
    $('export-diagnostics').disabled = !native || !logs || exportBusy;
    $('open-log-folder').disabled = !native || !logs || folderBusy;
    setText('export-diagnostics', exportBusy ? 'Exporting ZIP…' : 'Export diagnostic ZIP');
    $('open-export-folder').disabled = !native || exportFolderBusy;
    const input = ready && status.native_input ? status.input_diagnostics : null;
    setText('input-mode', !input ? 'Unavailable' : input.sending ? 'Sending input' : input.receiving ? 'Receiving input' : 'Local control');
    setText('input-facts', input ? `Raw input ${input.raw_input_registered ? 'registered' : 'unavailable'} · ${Number(input.monitor_count)} monitor${input.monitor_count === 1 ? '' : 's'} · ${Number(input.virtual_width)} × ${Number(input.virtual_height)} desktop · Per-monitor DPI ${input.per_monitor_dpi ? 'active' : 'unavailable'} · Remote cursor ${input.cursor_overlay_visible ? 'visible' : 'hidden'} · Hooks last installed: mouse ${input.mouse_hook_installed ? 'yes' : 'no'}, keyboard ${input.keyboard_hook_installed ? 'yes' : 'no'} (installation result only)` : 'Open the Windows desktop app to inspect the input engine.');
    const counters = $('input-counters'); counters.replaceChildren();
    for (const [field, label] of [
      ['source_edge_samples', 'Source edge checks'], ['source_edge_candidates', 'Eligible edge samples'],
      ['source_edge_intent_ready', 'Outward gestures ready'], ['source_enter_committed', 'Source entries committed'],
      ['remote_enter_committed', 'Remote entries committed'], ['remote_edge_returns', 'Returns through edge'],
      ['raw_motion_packets', 'Raw motion packets'], ['captured_moves', 'Forwarded moves'],
      ['captured_wheels', 'Forwarded wheel events'], ['injected_wheels', 'Injected wheel events'],
      ['injected_moves', 'Injected moves'], ['injection_failures', 'Injection failures'],
      ['hook_key_events', 'Local keyboard events'], ['captured_keys', 'Forwarded key events'],
      ['injected_keys', 'Injected key events'], ['ignored_remote_inputs', 'Inputs after routing ended'],
      ['queue_overflows', 'Queue overflows'], ['capture_resets', 'Capture resets'],
      ['raw_capture_stalls', 'Capture stalls'], ['cursor_overlay_activations', 'Cursor activations'],
      ['held_physical_keys', 'Local keys held'], ['held_physical_buttons', 'Local buttons held'],
      ['stale_keys_reconciled', 'Recovered key releases'], ['handoff_blocked_by_held', 'Handoffs blocked by held input'],
      ['hook_delivery_stalls', 'Suspected hook delivery stalls'], ['hook_recoveries', 'Hook recoveries'],
      ['hook_recovery_failures', 'Hook recovery failures'], ['raw_registration_repairs', 'Raw registration repairs'],
      ['raw_registration_failures', 'Raw registration failures'], ['device_removals_ignored', 'Unrelated removals ignored'],
      ['physical_mouse_reclaims', 'Local mouse reclaims'], ['mouse_jitter_events_ignored', 'Small mouse movements ignored'],
      ['protected_physical_keys', 'Keys awaiting physical release'], ['protected_physical_buttons', 'Buttons awaiting physical release'],
      ['cursor_overlay_failures', 'Cursor failures'], ['cursor_overlay_recoveries', 'Cursor recoveries'],
      ['raw_registration_query_failures', 'Raw ownership query failures'], ['raw_registration_mismatches', 'Raw ownership mismatches'],
      ['raw_absolute_packets', 'Unsupported absolute packets'], ['device_input_resets', 'Device removal releases'],
      ['native_commands', 'Native commands completed'], ['native_key_reclaims', 'Local key reclaims'],
      ['native_pump_slices', 'Native work slices'], ['native_pump_yields', 'Native work yields'],
      ['native_queue_wait_us', 'Total native queue wait (µs)'], ['native_queue_wait_peak_us', 'Longest native queue wait (µs)'],
      ['native_execution_us', 'Total native execution (µs)'], ['native_execution_peak_us', 'Longest native execution (µs)'],
      ['native_sendinput_calls', 'Windows input calls'], ['native_sendinput_total_us', 'Total Windows input time (µs)'],
      ['native_sendinput_peak_us', 'Longest Windows input call (µs)'], ['native_wheel_peak_us', 'Longest wheel input call (µs)'],
    ]) {
      const item = document.createElement('div');
      const term = document.createElement('dt'); term.textContent = label;
      const value = document.createElement('dd'); value.textContent = input ? Number(input[field] || 0).toLocaleString() : '—';
      item.append(term, value); counters.append(item);
    }
    const control = ready ? status.control_diagnostics : null;
    const pipeline = $('control-counters'); pipeline.replaceChildren();
    for (const [field, label, unit] of [
      ['outgoing_queue_peak', 'Peak send queue', ''], ['incoming_queue_peak', 'Peak input queue', ''],
      ['outgoing_moves_coalesced', 'Send moves combined', ''], ['incoming_moves_coalesced', 'Receive moves combined', ''],
      ['max_outgoing_queue_age_ms', 'Longest local send wait', ' ms'], ['max_incoming_queue_age_ms', 'Longest local input wait', ' ms'],
      ['max_native_apply_ms', 'Longest native apply', ' ms'], ['stale_input_failures', 'Stale input releases', ''],
      ['outgoing_queue_stale_failures', 'Source queue expired', ''], ['outgoing_write_deadline_failures', 'Send deadline exceeded', ''],
      ['incoming_queue_stale_failures', 'Receive queue expired', ''], ['native_completion_deadline_failures', 'Native completion timed out', ''],
    ]) {
      const item = document.createElement('div');
      const term = document.createElement('dt'); term.textContent = label;
      const value = document.createElement('dd'); value.textContent = control ? `${Number(control[field] || 0).toLocaleString()}${unit}` : '—';
      item.append(term, value); pipeline.append(item);
    }
  }

  function renderTroubleshooting() {
    const address = $('firewall-address');
    if (!firewallAddressEdited && status.peer?.address) address.value = status.peer.address;
    address.disabled = !native || firewallBusy;
    $('add-firewall-rule').disabled = !native || firewallBusy || !address.value.trim();
    $('add-firewall-rule').setAttribute('aria-busy', String(firewallBusy));
    setText('firewall-action-label', firewallBusy ? 'Waiting for Windows…' : 'Add a firewall rule');
    $('firewall-preview-note').hidden = native;
  }

  async function refresh() {
    if (!native || refreshing) return;
    refreshing = true;
    try {
      const raw = await invoke('get_status');
      status = normalizeStatus(raw);
      diagnosticsReady = Boolean(status.diagnostics);
      if (invitationRevealed && !status.invitation && status.invitation_seconds_left === 0) generatedInvitation = '';
      ready = true; engineError = '';
    } catch (error) {
      ready = false; engineError = errorText(error);
      // Diagnostics remain usable when identity loading or native startup fails.
      // This metadata command does not start a second engine or diagnostic writer.
      try {
        const diagnostics = await invoke('get_diagnostic_status');
        if (!diagnostics || typeof diagnostics.directory !== 'string') throw new Error('Unavailable');
        status.diagnostics = diagnostics; diagnosticsReady = true;
      } catch { diagnosticsReady = false; }
    }
    finally { refreshing = false; render(); }
  }

  async function saveSettings(changes) {
    if (!ready || settingsBusy) return;
    const next = { ...status.settings, ...changes };
    settingsBusy = true; render();
    try { status = normalizeStatus(await invoke('update_settings', { settings: next })); }
    catch (error) { toast(errorText(error), true); }
    finally { settingsBusy = false; render(); }
  }

  function navigate() {
    const desired = location.hash.replace('#', '');
    const page = ['devices', 'sharing', 'security', 'diagnostics', 'troubleshooting'].includes(desired) ? desired : 'devices';
    document.querySelectorAll('.page').forEach((section) => { section.hidden = section.id !== `page-${page}`; });
    document.querySelectorAll('[data-page]').forEach((link) => {
      const active = link.dataset.page === page;
      link.classList.toggle('active', active);
      if (active) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current');
    });
    setText('breadcrumb-page', page[0].toUpperCase() + page.slice(1));
    document.title = `${page[0].toUpperCase() + page.slice(1)} · Easy Mouse Connect`;
  }

  function selectPairTab(invite) {
    $('connect-tab').setAttribute('aria-selected', String(!invite));
    $('connect-tab').tabIndex = invite ? -1 : 0;
    $('invite-tab').setAttribute('aria-selected', String(invite));
    $('invite-tab').tabIndex = invite ? 0 : -1;
    $('connect-panel').hidden = invite;
    $('invite-panel').hidden = !invite;
    setText('pair-message', !native ? 'The browser preview cannot pair devices. Open the desktop app to continue.' : !ready ? 'The desktop engine is unavailable. Check Diagnostics before pairing.' : '');
  }

  function closePair() { if (!pairingBusy) $('pair-dialog').close(); }

  document.querySelectorAll('[data-open-pair]').forEach((button) => button.addEventListener('click', () => {
    pairSession += 1;
    previousFocus = document.activeElement;
    selectPairTab(false);
    $('pair-dialog').showModal();
    $('peer-address').focus();
  }));
  $('close-pair').addEventListener('click', closePair);
  $('pair-dialog').addEventListener('click', (event) => {
    if (event.target !== $('pair-dialog')) return;
    const box = $('pair-dialog').getBoundingClientRect();
    if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) closePair();
  });
  $('pair-dialog').addEventListener('cancel', (event) => { if (pairingBusy) event.preventDefault(); });
  $('pair-dialog').addEventListener('close', () => {
    pairSession += 1;
    invitationRevealed = false; generatedInvitation = '';
    $('invitation-code').value = ''; $('peer-invitation').value = '';
    renderInvitation();
    previousFocus?.focus();
  });
  $('connect-tab').addEventListener('click', () => selectPairTab(false));
  $('invite-tab').addEventListener('click', () => selectPairTab(true));
  document.querySelector('.pair-tabs').addEventListener('keydown', (event) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const invite = event.key === 'End' || (event.key !== 'Home' && $('invite-tab').getAttribute('aria-selected') !== 'true');
    selectPairTab(invite); $(invite ? 'invite-tab' : 'connect-tab').focus();
  });
  $('pair-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (!ready || pairingBusy) return;
    const address = $('peer-address').value.trim();
    const invitation = $('peer-invitation').value.trim();
    if (!address || !invitation) return;
    pairingBusy = true; render(); setText('pair-message', 'Verifying the pairing code and connecting…');
    try {
      await invoke('pair_device', { address, invitation });
      await refresh(); pairingBusy = false; $('pair-dialog').close();
      toast('Device paired. Its trusted identity is saved on this device.');
    } catch (error) { setText('pair-message', errorText(error)); }
    finally { pairingBusy = false; render(); }
  });
  $('create-invitation').addEventListener('click', async () => {
    if (!ready || invitationBusy) return;
    const session = pairSession;
    invitationBusy = true; render(); setText('pair-message', '');
    try {
      const invitation = await invoke('create_invitation');
      if (session !== pairSession || !$('pair-dialog').open) return;
      generatedInvitation = invitation;
      invitationRevealed = true;
      await refresh();
      $('invitation-code').focus(); $('invitation-code').select();
    } catch (error) { setText('pair-message', errorText(error)); }
    finally { invitationBusy = false; render(); }
  });
  $('copy-invitation').addEventListener('click', async () => {
    try { await invoke('copy_invitation'); toast('Pairing code copied. You can also type it on your other device.'); }
    catch (error) { $('invitation-code').focus(); $('invitation-code').select(); setText('pair-message', `${errorText(error)} Type the displayed code on your other device.`); }
  });
  $('peer-invitation').addEventListener('input', () => {
    const field = $('peer-invitation');
    const before = field.value.slice(0, field.selectionStart).replace(/[^a-z0-9]/gi, '').length;
    const characters = field.value.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 12);
    field.value = (characters.match(/.{1,4}/g) || []).join('-');
    const caret = Math.min(field.value.length, before + Math.max(0, Math.floor((before - 1) / 4)));
    field.setSelectionRange(caret, caret);
  });
  document.querySelectorAll('[data-setting]').forEach((input) => input.addEventListener('change', () => saveSettings({ [input.dataset.setting]: input.checked })));
  document.querySelectorAll('[data-peer-left]').forEach((button) => button.addEventListener('click', () => saveSettings({ peer_on_left: button.dataset.peerLeft === 'true' })));
  window.addEventListener('hashchange', navigate);
  $('refresh-status').addEventListener('click', refresh);
  $('reconnect-button').addEventListener('click', async () => {
    try { await invoke('reconnect'); await refresh(); toast('Reconnect requested.'); }
    catch (error) { toast(errorText(error), true); }
  });
  $('forget-button').addEventListener('click', async () => {
    // Forgetting revokes the saved trust and is reversible through a new invitation.
    try { await invoke('forget_peer'); selectedPath = ''; selectedName = ''; await refresh(); toast('Device forgotten. A new pairing code is required to pair again.'); }
    catch (error) { toast(errorText(error), true); }
  });
  $('emergency-stop').addEventListener('click', async () => {
    try { await invoke('emergency_stop'); await refresh(); toast('Desk sharing stopped.'); }
    catch (error) { toast(errorText(error), true); }
  });
  $('diagnostics-form').addEventListener('submit', async (event) => {
    event.preventDefault(); if (!ready || diagnosticsBusy) return;
    const address = $('diagnostic-address').value.trim(); if (!address) return;
    diagnosticsBusy = true;
    $('run-diagnostics').disabled = true; setText('diagnostics-output', 'Checking the address…');
    try { setText('diagnostics-output', await invoke('diagnostics', { address })); }
    catch (error) { setText('diagnostics-output', errorText(error)); }
    finally { diagnosticsBusy = false; $('run-diagnostics').disabled = !ready; }
  });
  $('project-link').addEventListener('click', async (event) => {
    if (!native) return;
    event.preventDefault();
    if (projectOpening) return;
    projectOpening = true;
    try { await invoke('open_project_url'); }
    catch (error) { toast(errorText(error), true); }
    finally { projectOpening = false; }
  });
  $('firewall-address').addEventListener('input', () => {
    firewallAddressEdited = true;
    renderTroubleshooting();
  });
  $('firewall-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (!native || firewallBusy) return;
    const address = $('firewall-address').value.trim();
    if (!address) return;
    // Rust validates the literal IP and selects the fixed program/port and helper.
    // The WebView can supply no executable, rule text, command or privilege option.
    firewallBusy = true; renderTroubleshooting();
    const result = $('firewall-result');
    result.hidden = false; result.classList.remove('error');
    setText('firewall-result', 'Waiting for Windows administrator approval…');
    try {
      setText('firewall-result', await invoke('add_peer_firewall_rule', { address }));
    } catch (error) {
      result.classList.add('error'); setText('firewall-result', errorText(error));
    } finally { firewallBusy = false; renderTroubleshooting(); }
  });
  $('export-diagnostics').addEventListener('click', async () => {
    if (!native || !diagnosticsReady || exportBusy) return;
    exportBusy = true; renderDiagnostics(); $('export-result').hidden = true;
    try {
      const path = await invoke('export_diagnostics');
      setText('export-path', path); $('export-result').hidden = false;
      toast('Diagnostic ZIP saved. Open its folder to copy or share the archive.');
    } catch (error) { toast(errorText(error), true); }
    finally { exportBusy = false; renderDiagnostics(); }
  });
  $('open-log-folder').addEventListener('click', async () => {
    if (!native || !diagnosticsReady || folderBusy) return;
    folderBusy = true; renderDiagnostics();
    try { await invoke('open_log_folder'); }
    catch (error) { toast(errorText(error), true); }
    finally { folderBusy = false; renderDiagnostics(); }
  });
  $('open-export-folder').addEventListener('click', async () => {
    if (!native || exportFolderBusy) return;
    exportFolderBusy = true; renderDiagnostics();
    try { await invoke('open_export_folder'); }
    catch (error) { toast(errorText(error), true); }
    finally { exportFolderBusy = false; renderDiagnostics(); }
  });
  $('dismiss-tray-hint').addEventListener('click', () => {
    $('tray-hint').hidden = true;
    try { localStorage.setItem('tray-hint-dismissed', '1'); } catch { /* Optional local UI preference. */ }
  });
  $('clear-file').addEventListener('click', () => { selectedPath = ''; selectedName = ''; renderTransfer(); });
  $('send-file').addEventListener('click', async () => {
    if (!canTransfer() || !selectedPath || transferBusy) return;
    transferBusy = true; renderTransfer();
    try {
      await invoke('send_file', { path: selectedPath });
      selectedPath = ''; selectedName = ''; toast('File offered. Your other device must approve the transfer.');
      await refresh();
    } catch (error) { toast(errorText(error), true); }
    finally { transferBusy = false; renderTransfer(); }
  });
  // Browser drops have no trusted filesystem path. The native file-drop event below supplies one.
  document.addEventListener('dragover', (event) => { event.preventDefault(); });
  document.addEventListener('drop', (event) => {
    event.preventDefault();
    if (!native) toast('Open the desktop app to send files.', true);
  });
  async function registerNativeDrop() {
    if (!native || typeof window.__TAURI__?.event?.listen !== 'function') return;
    try {
      await window.__TAURI__.event.listen('app://action-error', (event) => toast(String(event.payload), true));
      await window.__TAURI__.event.listen('tauri://drag-over', () => {
        if (canTransfer() && !$('page-sharing').hidden) $('drop-zone').classList.add('dragging');
      });
      await window.__TAURI__.event.listen('tauri://drag-leave', () => $('drop-zone').classList.remove('dragging'));
      await window.__TAURI__.event.listen('tauri://drag-drop', (event) => {
        $('drop-zone').classList.remove('dragging');
        if (transferBusy) { toast('Wait for the current offer to finish.', true); return; }
        if (!canTransfer()) { toast('Enable file transfers and connect your paired device first.', true); return; }
        const paths = event.payload?.paths;
        if (!Array.isArray(paths) || paths.length !== 1 || typeof paths[0] !== 'string') { toast('Drop one file at a time. Files must be under 100 MB.', true); return; }
        selectedPath = paths[0];
        selectedName = selectedPath.split(/[\\/]/).pop() || 'Selected file';
        location.hash = 'sharing'; renderTransfer();
      });
    } catch (error) { toast(`File drop could not start: ${errorText(error)}`, true); }
  }

  navigate(); render(); registerNativeDrop();
  try { $('tray-hint').hidden = !native || localStorage.getItem('tray-hint-dismissed') === '1'; }
  catch { $('tray-hint').hidden = !native; }
  if (native) {
    refresh();
    // Schedule after completion so a slow native call never creates an overlapping queue.
    (async function poll() {
      await new Promise((resolve) => setTimeout(resolve, 1000));
      if (!document.hidden) await refresh();
      poll();
    })();
    document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
  }
})();

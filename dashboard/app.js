const API = window.location.origin + '/api';
let selectedJobId = null;
let logPollTimer = null;
let logOffsets = { stdout: 0, stderr: 0, combined: 0 };
let currentJobs = [];
let sortCol = 'submitted_at';
let sortDir = 'desc';
let activeStream = 'both';
let activeFilter = 'all';

// ── Auth ──

function getApiKey() {
    // Session cookie handles dashboard auth automatically.
    // Manual key entry is a fallback if cookies are blocked.
    return localStorage.getItem('wartable_api_key');
}

function setApiKey(key) {
    localStorage.setItem('wartable_api_key', key);
}

function showAuthModal() {
    const overlay = document.getElementById('auth-overlay');
    overlay.style.display = 'flex';
    overlay.classList.add('visible');
    document.getElementById('auth-error').classList.remove('visible');
    document.getElementById('auth-key-input').value = '';
    document.getElementById('auth-key-input').focus();
}

function hideAuthModal() {
    const overlay = document.getElementById('auth-overlay');
    overlay.classList.remove('visible');
    overlay.style.display = 'none';
}

async function apiFetch(url, opts = {}) {
    const key = getApiKey();
    if (key) {
        opts.headers = opts.headers || {};
        opts.headers['X-API-Key'] = key;
    }
    const res = await fetch(url, opts);
    if (res.status === 401) {
        showAuthModal();
        throw new Error('Unauthorized');
    }
    return res;
}

document.getElementById('auth-submit').addEventListener('click', async () => {
    const key = document.getElementById('auth-key-input').value.trim();
    if (!key) return;
    // Test the key
    try {
        const res = await fetch(`${API}/resources`, { headers: { 'X-API-Key': key } });
        if (res.status === 401) {
            document.getElementById('auth-error').classList.add('visible');
            return;
        }
        setApiKey(key);
        hideAuthModal();
        fetchJobs();
        fetchResources();
    } catch {
        document.getElementById('auth-error').classList.add('visible');
    }
});

document.getElementById('auth-key-input').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') document.getElementById('auth-submit').click();
});

// ── DOM helpers ──

function el(tag, attrs, ...children) {
    const e = document.createElement(tag);
    if (attrs) {
        for (const [k, v] of Object.entries(attrs)) {
            if (k === 'className') e.className = v;
            else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
            else e.setAttribute(k, v);
        }
    }
    for (const c of children) {
        if (typeof c === 'string') e.appendChild(document.createTextNode(c));
        else if (c) e.appendChild(c);
    }
    return e;
}

function clearChildren(p) { while (p.firstChild) p.removeChild(p.firstChild); }

// ── Mobile scroll lock ──

function updateBodyScroll() {
    const panel = document.getElementById('detail-panel');
    const isMobile = window.matchMedia('(max-width: 768px)').matches;
    if (isMobile && !panel.classList.contains('hidden')) {
        document.body.style.overflow = 'hidden';
    } else {
        document.body.style.overflow = '';
    }
}

// ── Sorting ──

function getSortValue(job, col) {
    switch (col) {
        case 'status': return ['running','queued','failed','completed','cancelled'].indexOf(job.status);
        case 'name': return (job.name || job.job_id).toLowerCase();
        case 'submitted_at': return new Date(job.submitted_at).getTime();
        case 'duration': return getDurationSecs(job);
        case 'exit_code': return job.exit_code ?? -999;
        default: return '';
    }
}

function getDurationSecs(job) {
    if (!job.started_at) return -1;
    const end = job.completed_at ? new Date(job.completed_at) : new Date();
    return (end - new Date(job.started_at)) / 1000;
}

function sortJobs(jobs) {
    return [...jobs].sort((a, b) => {
        const va = getSortValue(a, sortCol);
        const vb = getSortValue(b, sortCol);
        let cmp = 0;
        if (typeof va === 'string') cmp = va.localeCompare(vb);
        else cmp = va - vb;
        return sortDir === 'asc' ? cmp : -cmp;
    });
}

function handleSort(col) {
    if (sortCol === col) {
        sortDir = sortDir === 'asc' ? 'desc' : 'asc';
    } else {
        sortCol = col;
        sortDir = col === 'submitted_at' ? 'desc' : 'asc';
    }
    updateSortIndicators();
    renderJobs(currentJobs);
}

function updateSortIndicators() {
    document.querySelectorAll('th.sortable').forEach(th => {
        th.classList.remove('sorted-asc', 'sorted-desc');
        if (th.dataset.sort === sortCol) {
            th.classList.add(sortDir === 'asc' ? 'sorted-asc' : 'sorted-desc');
        }
    });
}

// ── Job List ──

async function fetchJobs() {
    const params = activeFilter !== 'all' ? `?status=${activeFilter}` : '?limit=200';
    try {
        const res = await apiFetch(`${API}/jobs${params}`);
        currentJobs = await res.json();
        renderJobs(currentJobs);
        document.getElementById('connection-dot').classList.add('ok');
    } catch {
        document.getElementById('connection-dot').classList.remove('ok');
    }
}

function updateRow(tr, job) {
    const tds = tr.children;
    const exitText = job.exit_code !== null && job.exit_code !== undefined
        ? String(job.exit_code) : '-';
    const exitClass = job.exit_code === 0 ? 'exit-ok'
        : job.exit_code !== null && job.exit_code !== undefined ? 'exit-fail' : 'exit-na';

    // Status badge
    const badge = tds[0].firstChild;
    if (badge) {
        badge.className = `status-badge status-${job.status}`;
        badge.textContent = job.status;
    }
    tds[1].textContent = job.name || job.job_id.slice(0, 8);
    tds[2].textContent = job.command;
    tds[2].className = 'cmd-cell';
    tds[3].textContent = timeAgo(job.submitted_at);
    tds[4].textContent = duration(job);
    tds[5].textContent = exitText;
    tds[5].className = `exit-cell ${exitClass}`;
    const killable = ['queued', 'running'].includes(job.status);
    if (killable) {
        if (!tds[6].firstChild || tds[6].firstChild.tagName !== 'BUTTON') {
            clearChildren(tds[6]);
            tds[6].appendChild(el('button', {
                className: 'cancel-btn',
                onclick: (e) => { e.stopPropagation(); cancelJob(job.job_id); }
            }, 'KILL'));
        }
    } else {
        clearChildren(tds[6]);
    }
    tr.className = job.job_id === selectedJobId ? 'selected' : '';
}

function createRow(job) {
    const exitText = job.exit_code !== null && job.exit_code !== undefined
        ? String(job.exit_code) : '-';
    const exitClass = job.exit_code === 0 ? 'exit-ok'
        : job.exit_code !== null && job.exit_code !== undefined ? 'exit-fail' : 'exit-na';

    const tr = el('tr', {
            onclick: () => selectJob(job.job_id),
            className: job.job_id === selectedJobId ? 'selected' : ''
        },
        el('td', null, el('span', { className: `status-badge status-${job.status}` }, job.status)),
        el('td', null, job.name || job.job_id.slice(0, 8)),
        el('td', { className: 'cmd-cell' }, job.command),
        el('td', { className: 'time-cell' }, timeAgo(job.submitted_at)),
        el('td', { className: 'time-cell' }, duration(job)),
        el('td', { className: `exit-cell ${exitClass}` }, exitText),
        el('td', null,
            ['queued', 'running'].includes(job.status)
                ? el('button', {
                    className: 'cancel-btn',
                    onclick: (e) => { e.stopPropagation(); cancelJob(job.job_id); }
                }, 'KILL')
                : null
        )
    );
    tr.dataset.jobId = job.job_id;
    return tr;
}

function renderJobs(jobs) {
    const sorted = sortJobs(jobs);
    const tbody = document.getElementById('jobs-body');
    const empty = document.getElementById('empty-state');

    document.getElementById('job-count').textContent = jobs.length;

    if (sorted.length === 0) {
        clearChildren(tbody);
        empty.classList.remove('hidden');
        return;
    }
    empty.classList.add('hidden');

    // Build map of existing rows by job_id
    const existingRows = {};
    for (const tr of Array.from(tbody.children)) {
        if (tr.dataset.jobId) existingRows[tr.dataset.jobId] = tr;
    }

    // Build new row order, reusing existing DOM nodes
    const newRows = [];
    for (const job of sorted) {
        const existing = existingRows[job.job_id];
        if (existing) {
            updateRow(existing, job);
            delete existingRows[job.job_id];
            newRows.push(existing);
        } else {
            newRows.push(createRow(job));
        }
    }

    // Remove rows no longer in the list
    for (const tr of Object.values(existingRows)) {
        tr.remove();
    }

    // Reorder / insert into correct positions
    for (let i = 0; i < newRows.length; i++) {
        const current = tbody.children[i];
        if (current !== newRows[i]) {
            tbody.insertBefore(newRows[i], current || null);
        }
    }
}

// ── Job Detail ──

async function selectJob(jobId) {
    selectedJobId = jobId;
    document.getElementById('detail-panel').classList.remove('hidden');
    updateBodyScroll();
    logOffsets = { stdout: 0, stderr: 0, combined: 0 };
    clearChildren(document.getElementById('log-output'));

    try {
        const res = await apiFetch(`${API}/jobs/${jobId}`);
        const job = await res.json();
        renderDetail(job);
    } catch { /* ignore */ }

    if (logPollTimer) clearInterval(logPollTimer);
    pollLogs();
    logPollTimer = setInterval(pollLogs, 2000);
}

function renderDetail(job) {
    document.getElementById('detail-name').textContent = job.spec.name || job.id.slice(0, 12);
    const badge = document.getElementById('detail-status-badge');
    badge.textContent = job.status;
    badge.className = `status-badge status-${job.status}`;

    const meta = document.getElementById('detail-meta');
    clearChildren(meta);

    const add = (label, value) => {
        meta.appendChild(el('span', { className: 'meta-label' }, label));
        meta.appendChild(el('span', { className: 'meta-value' }, value));
    };

    const addCmd = (command) => {
        meta.appendChild(el('span', { className: 'meta-label' }, 'CMD'));
        const cmdVal = el('span', { className: 'meta-value meta-cmd collapsed' });
        const cmdText = el('pre', { className: 'cmd-text' }, command);
        cmdVal.appendChild(cmdText);
        if (command.length > 120 || command.split('\n').length > 2) {
            const toggle = el('button', {
                className: 'cmd-toggle',
                onclick: (e) => {
                    e.stopPropagation();
                    cmdVal.classList.toggle('collapsed');
                    toggle.textContent = cmdVal.classList.contains('collapsed') ? 'expand' : 'collapse';
                }
            }, 'expand');
            cmdVal.appendChild(toggle);
        } else {
            cmdVal.classList.remove('collapsed');
        }
        meta.appendChild(cmdVal);
    };

    add('ID', job.id);
    addCmd(job.spec.command);
    if (job.started_at) add('STARTED', new Date(job.started_at).toLocaleString());
    if (job.completed_at) add('ENDED', new Date(job.completed_at).toLocaleString());
    if (job.exit_code !== null && job.exit_code !== undefined) add('EXIT', String(job.exit_code));
    if (job.spec.tags && job.spec.tags.length) add('TAGS', job.spec.tags.join(', '));

    // Action buttons
    const actions = document.getElementById('detail-actions');
    clearChildren(actions);
    if (['failed', 'cancelled', 'completed'].includes(job.status)) {
        actions.appendChild(el('button', {
            className: 'action-btn retry-btn',
            onclick: () => retryJob(job.id),
        }, 'RETRY'));
    }
    if (['queued', 'running'].includes(job.status)) {
        actions.appendChild(el('button', {
            className: 'action-btn cancel-detail-btn',
            onclick: () => cancelJob(job.id),
        }, 'KILL'));
    }
}

// ── Logs ──

async function pollLogs() {
    if (!selectedJobId) return;
    try {
        if (activeStream === 'both') {
            const offset = logOffsets.combined || 0;
            const res = await apiFetch(`${API}/jobs/${selectedJobId}/logs?stream=both&since_offset=${offset}`);
            const data = await res.json();
            const newOffset = data.combined_offset || 0;
            if (newOffset > offset && data.combined) {
                for (const entry of data.combined) {
                    appendLog(entry.line, entry.stream === 'err' ? 'stderr' : 'stdout');
                }
                logOffsets.combined = newOffset;
            }
        } else {
            const offset = activeStream === 'stderr' ? logOffsets.stderr : logOffsets.stdout;
            const res = await apiFetch(`${API}/jobs/${selectedJobId}/logs?stream=${activeStream}&since_offset=${offset}`);
            const data = await res.json();
            if (activeStream === 'stdout' && data.stdout) {
                appendLog(data.stdout, 'stdout');
                logOffsets.stdout = data.stdout_offset || logOffsets.stdout;
            }
            if (activeStream === 'stderr' && data.stderr) {
                appendLog(data.stderr, 'stderr');
                logOffsets.stderr = data.stderr_offset || logOffsets.stderr;
            }
        }
    } catch { /* ignore */ }
}

function appendLog(text, stream) {
    if (!text) return;
    const output = document.getElementById('log-output');
    const span = document.createElement('span');
    span.className = `log-${stream}`;
    span.textContent = text;
    output.appendChild(span);
    if (document.getElementById('log-follow').checked) {
        output.scrollTop = output.scrollHeight;
    }
}

// ── Resources ──

async function fetchResources() {
    try {
        const res = await apiFetch(`${API}/resources`);
        const r = await res.json();

        document.getElementById('bar-cpu').style.width = r.cpu.usage_pct + '%';
        document.getElementById('val-cpu').textContent = r.cpu.usage_pct.toFixed(0) + '% / ' + r.cpu.cores + 'c';

        document.getElementById('bar-ram').style.width = r.ram.usage_pct.toFixed(0) + '%';
        document.getElementById('val-ram').textContent = r.ram.used_gb.toFixed(1) + '/' + r.ram.total_gb.toFixed(0) + 'G';

        document.getElementById('bar-disk').style.width = r.disk.usage_pct.toFixed(0) + '%';
        document.getElementById('val-disk').textContent = r.disk.used_gb.toFixed(0) + '/' + r.disk.total_gb.toFixed(0) + 'G';

        document.getElementById('val-load').textContent =
            r.load.one.toFixed(1) + '  ' + r.load.five.toFixed(1) + '  ' + r.load.fifteen.toFixed(1);

        renderGpus(r.gpus || []);
    } catch { /* ignore */ }
}

function renderGpus(gpus) {
    const strip = document.getElementById('gpu-strip');
    if (!gpus.length) { strip.classList.remove('visible'); return; }
    strip.classList.add('visible');
    clearChildren(strip);

    for (const gpu of gpus) {
        const vramPct = (gpu.vram_used_gb / gpu.vram_total_gb * 100).toFixed(0);
        const card = el('div', { className: 'gpu-card' },
            el('span', { className: 'gpu-idx' }, String(gpu.index)),
            el('span', { className: 'gpu-label' }, gpu.name.replace('NVIDIA ', '').replace('GeForce ', '')),
            el('div', { className: 'gpu-metrics' },
                el('span', { className: 'gpu-m' },
                    el('b', null, gpu.gpu_utilization_pct + '%')
                ),
                el('span', { className: 'gpu-m' },
                    el('div', { className: 'vram-bar' },
                        el('div', { className: 'vram-fill', style: 'width:' + vramPct + '%' })
                    ),
                    el('b', null, gpu.vram_used_gb.toFixed(1)),
                    '/' + gpu.vram_total_gb.toFixed(0) + 'G'
                ),
                el('span', { className: 'gpu-temp' }, gpu.temperature_c + '\u00B0'),
                gpu.power_draw_w
                    ? el('span', { className: 'gpu-power' }, gpu.power_draw_w.toFixed(0) + 'W')
                    : null
            )
        );
        strip.appendChild(card);
    }
}

// ── Clients ──

let clientsVisible = false;

async function fetchClients() {
    try {
        const res = await apiFetch(`${API}/clients`);
        const clients = await res.json();
        document.getElementById('client-count').textContent = clients.length;
        if (clientsVisible) renderClients(clients);
    } catch { /* ignore */ }
}

function renderClients(clients) {
    const list = document.getElementById('clients-list');
    clearChildren(list);
    if (!clients.length) {
        list.appendChild(el('div', { className: 'client-empty' }, 'No clients connected'));
        return;
    }
    for (const c of clients) {
        const rph = c.requests_per_hour != null
            ? (c.requests_per_hour < 10 ? c.requests_per_hour.toFixed(1) : Math.round(c.requests_per_hour))
            : '?';
        list.appendChild(el('div', { className: 'client-row' },
            el('span', { className: 'client-name' }, c.name),
            el('span', { className: 'client-meta' }, rph + ' req/hr'),
            el('span', { className: 'client-meta' }, timeAgo(c.last_seen) + ' ago'),
        ));
    }
}

document.getElementById('toggle-clients').addEventListener('click', () => {
    clientsVisible = !clientsVisible;
    const panel = document.getElementById('clients-panel');
    const toggle = document.getElementById('toggle-clients');
    if (clientsVisible) {
        panel.classList.remove('hidden');
        toggle.classList.add('active');
        fetchClients();
    } else {
        panel.classList.add('hidden');
        toggle.classList.remove('active');
    }
});

// ── Keys ──

let keysVisible = false;

async function fetchKeys() {
    try {
        const res = await apiFetch(`${API}/keys`);
        const keys = await res.json();
        if (keysVisible) renderKeys(keys);
    } catch { /* ignore */ }
}

function renderKeys(keys) {
    const list = document.getElementById('keys-list');
    clearChildren(list);
    for (const k of keys) {
        const row = el('div', { className: 'key-row' },
            el('span', { className: 'key-name' }, k.name),
            el('span', { className: 'key-prefix' }, k.key_prefix),
            el('span', { className: 'key-meta' }, timeAgo(k.created_at) + ' ago'),
        );
        if (!k.revocable) {
            const badge = k.name === 'admin' ? 'key-badge-admin' : 'key-badge-config';
            row.appendChild(el('span', { className: `key-badge ${badge}` }, k.name === 'admin' ? 'admin' : 'config'));
        } else {
            row.appendChild(el('button', {
                className: 'revoke-btn',
                onclick: () => revokeKey(k.name),
            }, 'REVOKE'));
        }
        list.appendChild(row);
    }
}

async function revokeKey(name) {
    try {
        await apiFetch(`${API}/keys/revoke`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name }),
        });
        fetchKeys();
    } catch { /* ignore */ }
}

document.getElementById('generate-key-btn').addEventListener('click', async () => {
    const input = document.getElementById('new-key-name');
    const name = input.value.trim();
    if (!name) return;
    try {
        const res = await apiFetch(`${API}/keys/generate`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name }),
        });
        const data = await res.json();
        document.getElementById('new-key-value').textContent = data.key;
        document.getElementById('new-key-display').classList.remove('hidden');
        input.value = '';
        fetchKeys();
    } catch { /* ignore */ }
});

document.getElementById('toggle-keys').addEventListener('click', () => {
    keysVisible = !keysVisible;
    const panel = document.getElementById('keys-panel');
    const toggle = document.getElementById('toggle-keys');
    if (keysVisible) {
        panel.classList.remove('hidden');
        toggle.classList.add('active');
        fetchKeys();
    } else {
        panel.classList.add('hidden');
        toggle.classList.remove('active');
        document.getElementById('new-key-display').classList.add('hidden');
    }
});

// ── Actions ──

async function cancelJob(jobId) {
    await apiFetch(`${API}/jobs/${jobId}/cancel`, { method: 'POST' });
    fetchJobs();
}

async function retryJob(jobId) {
    try {
        const res = await apiFetch(`${API}/jobs/${jobId}/retry`, { method: 'POST' });
        const data = await res.json();
        fetchJobs();
        selectJob(data.new_job_id);
    } catch { /* ignore */ }
}

function copyLogs() {
    const output = document.getElementById('log-output');
    const text = output.textContent;
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
        const btn = document.getElementById('copy-logs-btn');
        btn.textContent = 'COPIED';
        setTimeout(() => { btn.textContent = 'COPY'; }, 1500);
    });
}

// ── Helpers ──

function timeAgo(ts) {
    const diff = (Date.now() - new Date(ts).getTime()) / 1000;
    if (diff < 60) return Math.floor(diff) + 's';
    if (diff < 3600) return Math.floor(diff / 60) + 'm';
    if (diff < 86400) return Math.floor(diff / 3600) + 'h';
    return Math.floor(diff / 86400) + 'd';
}

function duration(job) {
    if (!job.started_at) return '-';
    const end = job.completed_at ? new Date(job.completed_at) : new Date();
    const secs = (end - new Date(job.started_at)) / 1000;
    if (secs < 60) return Math.floor(secs) + 's';
    if (secs < 3600) return Math.floor(secs / 60) + 'm ' + Math.floor(secs % 60) + 's';
    return Math.floor(secs / 3600) + 'h ' + Math.floor((secs % 3600) / 60) + 'm';
}

// ── Init ──

// Sort headers
document.querySelectorAll('th.sortable').forEach(th => {
    th.addEventListener('click', () => handleSort(th.dataset.sort));
});
updateSortIndicators();

// Filter pills
document.querySelectorAll('.pill').forEach(btn => {
    btn.addEventListener('click', () => {
        document.querySelectorAll('.pill').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        activeFilter = btn.dataset.status;
        fetchJobs();
    });
});

// Log stream tabs
document.querySelectorAll('.log-tab').forEach(tab => {
    tab.addEventListener('click', () => {
        document.querySelectorAll('.log-tab').forEach(t => t.classList.remove('active'));
        tab.classList.add('active');
        activeStream = tab.dataset.stream;
        logOffsets = { stdout: 0, stderr: 0, combined: 0 };
        clearChildren(document.getElementById('log-output'));
        pollLogs();
    });
});

// Copy logs
document.getElementById('copy-logs-btn').addEventListener('click', copyLogs);

// Close detail
document.getElementById('close-detail').onclick = () => {
    document.getElementById('detail-panel').classList.add('hidden');
    selectedJobId = null;
    if (logPollTimer) clearInterval(logPollTimer);
    updateBodyScroll();
};

// Responsive scroll lock on resize
window.addEventListener('resize', updateBodyScroll);

// Go
fetchJobs();
fetchResources();
fetchClients();
setInterval(fetchJobs, 3000);
setInterval(fetchResources, 5000);
setInterval(fetchClients, 5000);

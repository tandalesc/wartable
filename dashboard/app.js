const API = window.location.origin + '/api';
let selectedJobId = null;
let logPollTimer = null;
let logOffsets = { stdout: 0, stderr: 0 };

// --- Safe DOM helpers ---

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

function clearChildren(parent) {
    while (parent.firstChild) parent.removeChild(parent.firstChild);
}

// --- Job List ---

async function fetchJobs() {
    const status = document.getElementById('status-filter').value;
    const params = status !== 'all' ? `?status=${status}` : '';
    try {
        const res = await fetch(`${API}/jobs${params}`);
        const jobs = await res.json();
        renderJobs(jobs);
        const conn = document.getElementById('connection-status');
        conn.textContent = 'connected';
        conn.className = 'status-connected';
    } catch {
        const conn = document.getElementById('connection-status');
        conn.textContent = 'disconnected';
        conn.className = 'status-disconnected';
    }
}

function renderJobs(jobs) {
    const tbody = document.getElementById('jobs-body');
    clearChildren(tbody);

    for (const job of jobs) {
        const tr = el('tr', {
            onclick: () => selectJob(job.job_id),
            className: job.job_id === selectedJobId ? 'selected' : ''
        },
            el('td', null,
                el('span', { className: `status-badge status-${job.status}` }, job.status)
            ),
            el('td', null, job.name || job.job_id.slice(0, 8)),
            el('td', { className: 'command-preview' }, job.command),
            el('td', null, timeAgo(job.submitted_at)),
            el('td', null, duration(job)),
            el('td', null,
                ['queued', 'running'].includes(job.status)
                    ? el('button', {
                        className: 'cancel-btn',
                        onclick: (e) => { e.stopPropagation(); cancelJob(job.job_id); }
                    }, 'cancel')
                    : null
            )
        );
        tbody.appendChild(tr);
    }
}

// --- Job Detail ---

async function selectJob(jobId) {
    selectedJobId = jobId;
    document.getElementById('detail-panel').classList.remove('hidden');
    logOffsets = { stdout: 0, stderr: 0 };
    clearChildren(document.getElementById('log-output'));

    try {
        const res = await fetch(`${API}/jobs/${jobId}`);
        const job = await res.json();
        renderDetail(job);
    } catch { /* ignore */ }

    if (logPollTimer) clearInterval(logPollTimer);
    pollLogs();
    logPollTimer = setInterval(pollLogs, 2000);
}

function metaRow(label, value) {
    return el('div', { className: 'meta-row' },
        el('span', { className: 'meta-label' }, label),
        typeof value === 'string' ? el('span', null, value) : value
    );
}

function renderDetail(job) {
    const meta = document.getElementById('detail-meta');
    clearChildren(meta);

    meta.appendChild(metaRow('ID', job.id));
    meta.appendChild(metaRow('Status',
        el('span', { className: `status-badge status-${job.status}` }, job.status)
    ));
    meta.appendChild(metaRow('Command',
        el('span', { style: 'font-family:monospace' }, job.spec.command)
    ));
    if (job.started_at) {
        meta.appendChild(metaRow('Started', new Date(job.started_at).toLocaleString()));
    }
    if (job.exit_code !== null && job.exit_code !== undefined) {
        meta.appendChild(metaRow('Exit Code', String(job.exit_code)));
    }
}

async function pollLogs() {
    if (!selectedJobId) return;
    const stream = document.querySelector('input[name="log-stream"]:checked').value;
    try {
        if (stream === 'both') {
            // Fetch stdout and stderr separately to track offsets independently
            const [outRes, errRes] = await Promise.all([
                fetch(`${API}/jobs/${selectedJobId}/logs?stream=stdout&since_offset=${logOffsets.stdout}`),
                fetch(`${API}/jobs/${selectedJobId}/logs?stream=stderr&since_offset=${logOffsets.stderr}`)
            ]);
            const outData = await outRes.json();
            const errData = await errRes.json();

            if (outData.stdout) appendLog(outData.stdout, 'stdout');
            if (errData.stderr) appendLog(errData.stderr, 'stderr');

            if (outData.stdout_offset > logOffsets.stdout) logOffsets.stdout = outData.stdout_offset;
            if (errData.stderr_offset > logOffsets.stderr) logOffsets.stderr = errData.stderr_offset;
        } else {
            const offset = stream === 'stderr' ? logOffsets.stderr : logOffsets.stdout;
            const res = await fetch(`${API}/jobs/${selectedJobId}/logs?stream=${stream}&since_offset=${offset}`);
            const data = await res.json();

            if (data.stdout) appendLog(data.stdout, 'stdout');
            if (data.stderr) appendLog(data.stderr, 'stderr');

            if (data.stdout_offset > logOffsets.stdout) logOffsets.stdout = data.stdout_offset;
            if (data.stderr_offset > logOffsets.stderr) logOffsets.stderr = data.stderr_offset;
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

// --- Resources ---

async function fetchResources() {
    try {
        const res = await fetch(`${API}/resources`);
        const r = await res.json();

        document.getElementById('bar-cpu').style.width = r.cpu.usage_pct + '%';
        document.getElementById('val-cpu').textContent = r.cpu.usage_pct.toFixed(0) + '% (' + r.cpu.cores + ' cores)';

        document.getElementById('bar-ram').style.width = r.ram.usage_pct.toFixed(0) + '%';
        document.getElementById('val-ram').textContent = r.ram.used_gb.toFixed(1) + ' / ' + r.ram.total_gb.toFixed(1) + ' GB';

        document.getElementById('bar-disk').style.width = r.disk.usage_pct.toFixed(0) + '%';
        document.getElementById('val-disk').textContent = r.disk.used_gb.toFixed(0) + ' / ' + r.disk.total_gb.toFixed(0) + ' GB';

        document.getElementById('val-load').textContent = r.load.one.toFixed(1) + ' / ' + r.load.five.toFixed(1) + ' / ' + r.load.fifteen.toFixed(1);

        renderGpus(r.gpus || []);
    } catch { /* ignore */ }
}

function renderGpus(gpus) {
    const container = document.getElementById('gpu-bar');
    if (gpus.length === 0) {
        container.classList.remove('visible');
        return;
    }
    container.classList.add('visible');
    clearChildren(container);

    for (const gpu of gpus) {
        const vramPct = (gpu.vram_used_gb / gpu.vram_total_gb * 100).toFixed(0);
        const card = el('div', { className: 'gpu-card' },
            el('span', { className: 'gpu-name' }, 'GPU ' + gpu.index),
            el('span', { style: 'color:#8b949e;font-size:11px' }, gpu.name),
            el('span', { className: 'gpu-stat' },
                'Util ',
                el('span', null, gpu.gpu_utilization_pct + '%')
            ),
            el('span', { className: 'gpu-stat' },
                'VRAM ',
                el('div', { className: 'bar-container' },
                    el('div', { className: 'bar bar-vram', style: 'width:' + vramPct + '%' })
                ),
                el('span', null, gpu.vram_used_gb.toFixed(1) + '/' + gpu.vram_total_gb.toFixed(0) + 'G')
            ),
            el('span', { className: 'gpu-temp' }, gpu.temperature_c + '\u00B0C'),
            gpu.power_draw_w
                ? el('span', { className: 'gpu-power' }, gpu.power_draw_w.toFixed(0) + 'W')
                : null
        );
        container.appendChild(card);
    }
}

// --- Actions ---

async function cancelJob(jobId) {
    await fetch(`${API}/jobs/${jobId}/cancel`, { method: 'POST' });
    fetchJobs();
}

// --- Helpers ---

function timeAgo(ts) {
    const diff = (Date.now() - new Date(ts).getTime()) / 1000;
    if (diff < 60) return `${Math.floor(diff)}s ago`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
    return `${Math.floor(diff / 86400)}d ago`;
}

function duration(job) {
    if (!job.started_at) return '-';
    const end = job.completed_at ? new Date(job.completed_at) : new Date();
    const secs = (end - new Date(job.started_at)) / 1000;
    if (secs < 60) return `${Math.floor(secs)}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${Math.floor(secs % 60)}s`;
    return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

// --- Init ---

document.getElementById('close-detail').onclick = () => {
    document.getElementById('detail-panel').classList.add('hidden');
    selectedJobId = null;
    if (logPollTimer) clearInterval(logPollTimer);
};

document.getElementById('status-filter').onchange = fetchJobs;
document.getElementById('refresh-btn').onclick = fetchJobs;

fetchJobs();
fetchResources();
setInterval(fetchJobs, 3000);
setInterval(fetchResources, 5000);

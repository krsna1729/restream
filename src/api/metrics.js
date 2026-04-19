const fs = require('fs');
const os = require('os');
const { errMsg } = require('../utils/app');

function getCpuTotals() {
    const totals = os.cpus().reduce(
        (acc, cpu) => {
            const times = cpu.times || {};
            const total =
                Number(times.user || 0) +
                Number(times.nice || 0) +
                Number(times.sys || 0) +
                Number(times.idle || 0) +
                Number(times.irq || 0);
            acc.total += total;
            acc.idle += Number(times.idle || 0);
            return acc;
        },
        { total: 0, idle: 0 },
    );
    return totals;
}

function getNetworkTotals() {
    try {
        const content = fs.readFileSync('/proc/net/dev', 'utf8');
        const lines = content.split('\n').slice(2).filter(Boolean);
        let rx = 0;
        let tx = 0;

        for (const line of lines) {
            const [ifaceRaw, rest] = line.split(':');
            if (!ifaceRaw || !rest) continue;
            const iface = ifaceRaw.trim();
            if (!iface || iface === 'lo') continue;

            const fields = rest.trim().split(/\s+/);
            if (fields.length < 16) continue;

            rx += Number(fields[0] || 0);
            tx += Number(fields[8] || 0);
        }

        return { rx, tx };
    } catch (err) {
        return { rx: 0, tx: 0 };
    }
}

function getDiskUsage(pathname = '/') {
    try {
        const stats = fs.statfsSync(pathname);
        const blockSize = Number(stats.bsize || 0);
        const totalBlocks = Number(stats.blocks || 0);
        const availBlocks = Number(stats.bavail || stats.bfree || 0);

        const totalBytes = blockSize * totalBlocks;
        const freeBytes = blockSize * availBlocks;
        const usedBytes = Math.max(0, totalBytes - freeBytes);
        const usedPercent = totalBytes > 0 ? (usedBytes / totalBytes) * 100 : null;

        return { totalBytes, usedBytes, freeBytes, usedPercent };
    } catch (err) {
        return {
            totalBytes: null,
            usedBytes: null,
            freeBytes: null,
            usedPercent: null,
        };
    }
}

function registerSystemMetricsApi({ app }) {
    let systemMetricsSample = {
        ts: Date.now(),
        cpu: getCpuTotals(),
        net: getNetworkTotals(),
    };

    app.get('/metrics/system', (req, res) => {
        try {
            const now = Date.now();
            const dtSec = Math.max((now - systemMetricsSample.ts) / 1000, 0.001);

            const currentCpu = getCpuTotals();
            const currentNet = getNetworkTotals();
            const memTotal = os.totalmem();
            const memFree = os.freemem();
            const memUsed = Math.max(0, memTotal - memFree);
            const memUsedPercent = memTotal > 0 ? (memUsed / memTotal) * 100 : null;
            const disk = getDiskUsage('/');

            const cpuTotalDiff = currentCpu.total - systemMetricsSample.cpu.total;
            const cpuIdleDiff = currentCpu.idle - systemMetricsSample.cpu.idle;
            let cpuUsagePercent = 0;
            if (cpuTotalDiff > 0) {
                cpuUsagePercent = Math.max(
                    0,
                    Math.min(100, ((cpuTotalDiff - cpuIdleDiff) / cpuTotalDiff) * 100),
                );
            }

            const rxDiff = Math.max(0, currentNet.rx - systemMetricsSample.net.rx);
            const txDiff = Math.max(0, currentNet.tx - systemMetricsSample.net.tx);
            const downloadBytesPerSec = rxDiff / dtSec;
            const uploadBytesPerSec = txDiff / dtSec;

            systemMetricsSample = {
                ts: now,
                cpu: currentCpu,
                net: currentNet,
            };

            return res.json({
                generatedAt: new Date(now).toISOString(),
                cpu: {
                    usagePercent: Number(cpuUsagePercent.toFixed(2)),
                    cores: os.cpus().length,
                    load1: Number(os.loadavg()[0].toFixed(2)),
                },
                memory: {
                    totalBytes: memTotal,
                    usedBytes: memUsed,
                    freeBytes: memFree,
                    usedPercent: memUsedPercent !== null ? Number(memUsedPercent.toFixed(2)) : null,
                },
                disk,
                network: {
                    downloadBytesPerSec: Number(downloadBytesPerSec.toFixed(2)),
                    uploadBytesPerSec: Number(uploadBytesPerSec.toFixed(2)),
                    downloadKbps: Number(((downloadBytesPerSec * 8) / 1000).toFixed(2)),
                    uploadKbps: Number(((uploadBytesPerSec * 8) / 1000).toFixed(2)),
                },
            });
        } catch (err) {
            return res.status(500).json({ error: errMsg(err) });
        }
    });
}

module.exports = { registerSystemMetricsApi };

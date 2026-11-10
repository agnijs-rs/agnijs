// The clock and the local zone come from the host (now_ms,
// time_zone_offset_ms, time_zone_name).
var out = [];
const now = new Date();
out.push(String(Date.now()));
out.push(String(now.getTime()));
out.push(now.toISOString());
out.push(now.toString());
out.push(now.toUTCString());
out.push([now.getHours(), now.getMinutes(), now.getTimezoneOffset()].join(","));
out.push(String(new Date(2024, 0, 31, 12, 30).getTime()));
out.push(String(Date.parse("2024-02-29T23:59:59")));
out.push(String(Date.parse("2024-02-29T23:59:59Z")));
out.push(new Date(0).toDateString());
out.push(JSON.stringify({ when: new Date(86400000) }));
out.push(new Date(1e15).toLocaleDateString("en-US", { timeZone: "UTC" }));

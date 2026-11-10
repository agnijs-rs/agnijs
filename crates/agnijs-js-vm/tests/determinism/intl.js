// Intl, with the host's default locale (en-US) and time zone (UTC). The
// locale data is compiled into the build.
var out = [];
out.push(new Intl.NumberFormat().format(1234567.891));
out.push(new Intl.NumberFormat("en-US", { style: "currency", currency: "EUR" }).format(-1234.5));
out.push(new Intl.NumberFormat("de-DE", { maximumFractionDigits: 1 }).format(9876.54));
out.push(new Intl.DateTimeFormat().format(new Date(0)));
out.push(new Intl.DateTimeFormat("en-GB", { dateStyle: "full", timeStyle: "long", timeZone: "UTC" }).format(new Date(1e12)));
out.push(new Intl.DateTimeFormat().resolvedOptions().timeZone + " " + new Intl.NumberFormat().resolvedOptions().locale);
out.push(["one", "two", "few"].map((w, i) => new Intl.PluralRules("en-US").select(i)).join(","));
out.push((1234.5).toLocaleString());
out.push(new Date(0).toLocaleString());

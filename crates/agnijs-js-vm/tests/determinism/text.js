// Strings, Unicode data and regular expressions (compiled into the build).
var out = [];
out.push("a-b_c d".replace(/[-_ ]([a-z])/g, (_, c) => c.toUpperCase()));
out.push([..."x1y22z333".matchAll(/(\d+)/g)].map((m) => m[1] + "@" + m.index).join(","));
out.push("one, two,three ,four".split(/\s*,\s*/).join("|"));
out.push(["straße".toUpperCase(), "İ".toLowerCase().length, "ǅ".toLowerCase(), "ﬀ".toUpperCase()].join(" "));
out.push(["é".normalize("NFD").length, "ẛ̣".normalize("NFKC").codePointAt(0)].join(" "));
out.push(/(?<year>\d{4})-(?<m>\d\d)/u.exec("on 2024-07").groups.year);
out.push(/\p{Script=Greek}+/u.exec("abc αβγ def")[0]);
out.push("aaaaaaaaaaaaaaaaaaaaaaaaaaaa!".match(/^(a+)+$/) === null ? "no match" : "match");
out.push(String.raw`a\n${1 + 1}b`);
out.push("abc".padStart(6, "12") + "|" + "abc".padEnd(5) + "|");
out.push(encodeURIComponent("a b&c=d/é") + " " + escape("ä+"));
out.push([..."👍🏽x"].length + " " + "👍🏽x".length);
out.push(JSON.stringify("\u0000\u001f\ud834"));

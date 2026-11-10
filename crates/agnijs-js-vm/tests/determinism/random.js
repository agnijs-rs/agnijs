// Math.random follows the host's seed (Host::random_seed).
var out = [];
for (let i = 0; i < 6; i++) out.push(String(Math.random()));
const ints = [];
for (let i = 0; i < 12; i++) ints.push(Math.floor(Math.random() * 1000));
out.push(ints.join(","));
const deck = Array.from({ length: 16 }, (_, i) => i);
for (let i = deck.length - 1; i > 0; i--) {
  const j = Math.floor(Math.random() * (i + 1));
  [deck[i], deck[j]] = [deck[j], deck[i]];
}
out.push(deck.join(","));
// Code compiled from a string draws from the same stream.
const other = new Function("return Math.random()");
out.push(String(other()));

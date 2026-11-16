// Map columns count UTF-16 units: 𝒳 is two, é one. A raw U+2028 in a
// string ends a line for the engine's positions, so the map must agree.
const 𝒳: string = "é😀", after: number = 1;
const separator = "a b";
export const 名前: string = `${𝒳}${after}${separator}`;

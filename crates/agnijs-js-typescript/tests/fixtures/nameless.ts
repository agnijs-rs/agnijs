// Oxc's parser skips "Expected function name" when type parameters follow
// `function`. Only `export default` may leave a declaration unnamed.
function<T>(x: T) {}
export function<T>(x: T) {}
{ async function*<T>() {} }
export default function <T>(x: T) { return x; }

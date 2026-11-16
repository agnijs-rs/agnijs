// @options: decorators
// Legacy decorators apply to class declarations and their members only.
function mark(target: any, key?: string) {}
export const A = @mark class {};
export const B = class { @mark m() {} };

// .cts is a script: no import or export, var at the top is global.
var counter: number = 0;
function next(step: number = 1): number {
  return (counter += step);
}
enum Mode {
  Fast,
  Slow,
}
next(Mode.Slow);

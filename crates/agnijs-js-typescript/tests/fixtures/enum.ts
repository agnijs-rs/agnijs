export enum Direction {
  Up = 1,
  Down,
  Left = Up << 4,
  Right = "right".length,
}
export enum Label {
  Yes = "yes",
  No = `no`,
}
export const enum Inline {
  A,
  B = A + 10,
}
declare enum Ambient {
  X,
}
enum Direction {
  Center = 100,
}
export const values = [Direction.Up, Direction[2], Label.No, Inline.B];

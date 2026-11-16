// Type-level syntax only: all of it is erased.
interface Point<T = number> {
  readonly x: T;
  y?: T;
  [key: string]: unknown;
}
type Pair<A, B> = [first: A, second?: B];
type Mapped<T> = { -readonly [K in keyof T as `get${Capitalize<string & K>}`]-?: () => T[K] };
declare const ambient: unique symbol;
declare function external(x: number): string;
declare class Outside {
  method(): void;
}

function overloaded(x: string): string;
function overloaded(x: number): number;
function overloaded(x: unknown): unknown {
  return x;
}

abstract class Shape implements Point {
  [key: string]: unknown;
  readonly x: number = 0;
  declare y?: number;
  private secret!: string;
  protected static count: number;
  abstract area(): number;
  public describe(this: Shape, prefix?: string): string {
    return `${prefix ?? ""}${this.area()}`;
  }
}

class Square extends Shape {
  constructor(private readonly side: number) {
    super();
  }
  override area(): number {
    return this.side! ** 2;
  }
}

const p = { x: 1, y: 2 } satisfies Point;
const n = (p as unknown as Point<number>).x;
const legacy = <number>(<unknown>p.y);
const instantiated = overloaded<string>;
let definite!: number;
export function assert(value: unknown): asserts value is string {}
export { Square, p, n, legacy, instantiated, definite };
export type { Point, Pair, Mapped };

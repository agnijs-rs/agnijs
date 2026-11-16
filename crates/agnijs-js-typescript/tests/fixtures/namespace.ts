export namespace Geometry {
  export const unit = 1;
  export function double(x: number): number {
    return x * 2 * unit;
  }
  export namespace Deep.Deeper {
    export const level = 3;
  }
}
export namespace Geometry {
  // Names another block exports are qualified (see namespace-merged.ts).
  export const merged = Geometry.double(Geometry.unit);
}
// Types only: emits nothing.
namespace Types {
  export interface Shape {}
  export type Id = string;
}
declare namespace Ambient {
  const x: number;
}
import level = Geometry.Deep.Deeper.level;
export const fromAlias = level;

export class Account {
  balance = 0;
  constructor(
    public readonly id: string,
    private owner: string,
    protected override_?: boolean,
    plain = 1,
  ) {
    this.balance = plain;
  }
}
class Base {
  constructor(public name: string) {}
}
export class Child extends Base {
  extra = "x";
  constructor(name: string, public age: number) {
    super(name);
  }
}

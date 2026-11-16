// @options: decorators metadata
function sealed(target: Function) {
  Object.seal(target);
}
function log(target: object, key: string, desc: PropertyDescriptor) {
  return desc;
}
function inject(target: object, key: string | undefined, index: number) {}

@sealed
export class Service {
  @log
  run(@inject name: string, count: number): boolean {
    return name.length > count;
  }
  @log static make(): Service {
    return new Service();
  }
}

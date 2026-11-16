// @options: factory=h fragment=Fragment
import { h, Fragment } from "./dom.js";
export const view = (items: string[]) => (
  <ul>
    {items.map((item) => <li>{item}</li>)}
    <></>
  </ul>
);

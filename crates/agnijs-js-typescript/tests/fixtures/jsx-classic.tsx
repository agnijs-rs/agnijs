import * as React from "react";
interface Props {
  name: string;
}
export function Hello({ name }: Props) {
  return (
    <>
      <h1 className="title">Hello, {name}!</h1>
      <Greeting {...{ name }} key="k" />
      {/* a comment */}
      <input disabled value={name as string} />
    </>
  );
}
function Greeting(props: Props) {
  return <p>{props.name}</p>;
}

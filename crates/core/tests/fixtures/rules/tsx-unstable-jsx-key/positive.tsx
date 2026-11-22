import React from 'react';
export function Rows({items}: {items: string[]}) {
    return <section>{items.map(label => <span key={Math.random()}>{label}</span>)}</section>;
}

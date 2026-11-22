import React from 'react';
export const Rows = ({items}) => items.map(item => <div key={Math.random()}>{item.title}</div>);

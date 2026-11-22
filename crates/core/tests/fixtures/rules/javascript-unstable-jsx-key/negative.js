import React from 'react';
export const Rows = ({items}) => items.map(item => <div key={item.id}>{item.title}</div>);

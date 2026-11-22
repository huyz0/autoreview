import { useState, useId } from 'react';

function Demo(attached) {
    const ids = [];
    for (let i = 0; i < 3; i++) {
        ids.push(useId());
    }
    return attached ? ids.join(",") : "";
}

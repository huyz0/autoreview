import { useState, use } from 'react';
import { useId } from './ids';

function Demo(attached) {
    const state = useState(0);
    for (let i = 0; i < 3; i++) {
        useId(i);
        use(ThemeContext);
    }
    return attached ? state[0] : 0;
}

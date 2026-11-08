(() => {
    // Expected values are Chromium's getComputedStyle() serializations.
    const cases = [
        ['border-radius:18px', 'border-radius', '18px'],
        ['border-radius:1px 2px 3px 4px / 5px 6px 7px 8px', 'border-radius', '1px 2px 3px 4px / 5px 6px 7px 8px'],
        ['border-radius:10px 20px', 'border-radius', '10px 20px'],
        ['border-top-left-radius:3px 5px', 'border-radius', '3px 0px 0px / 5px 0px 0px'],
        ['border-radius:5px;border-bottom-right-radius:2px', 'border-radius', '5px 5px 2px'],
        ['', 'border-radius', '0px'],
        ['gap:4px 8px', 'gap', '4px 8px'],
        ['gap:5px', 'gap', '5px'],
        ['row-gap:7px', 'gap', '7px normal'],
        ['', 'gap', 'normal'],
        ['flex:1 1 0%', 'flex', '1 1 0%'],
        ['flex:2', 'flex', '2 1 0%'],
        ['flex:none', 'flex', '0 0 auto'],
        ['flex:auto', 'flex', '1 1 auto'],
        ['flex:1 2 3px', 'flex', '1 2 3px'],
        ['', 'flex', '0 1 auto'],
        ['outline:1px solid blue', 'outline', 'rgb(0, 0, 255) solid 1px'],
        ['outline:auto', 'outline', 'rgb(0, 0, 0) auto 3px'],
        ['color:red;outline:2px solid currentcolor', 'outline', 'rgb(255, 0, 0) solid 2px'],
        ['', 'outline', 'rgb(0, 0, 0) none 3px'],
        ['display:grid;place-items:center', 'place-items', 'center'],
        ['place-items:start end', 'place-items', 'start end'],
        ['justify-items:legacy center', 'place-items', 'normal legacy center'],
        ['', 'place-items', 'normal'],
        ['place-content:center start', 'place-content', 'center start'],
        ['place-content:space-between', 'place-content', 'space-between'],
        ['justify-content:space-evenly', 'place-content', 'normal space-evenly'],
        ['', 'place-content', 'normal'],
        ['place-self:end', 'place-self', 'end'],
        ['place-self:center stretch', 'place-self', 'center stretch'],
        ['align-self:last baseline;justify-self:safe end', 'place-self', 'last baseline safe end'],
        ['', 'place-self', 'auto'],
        ['grid-area:1/2/3/4', 'grid-area', '1 / 2 / 3 / 4'],
        ['grid-row:1/span 2;grid-column:3', 'grid-area', '1 / 3 / span 2'],
        ['grid-area:a/b', 'grid-area', 'a / b'],
        ['', 'grid-area', 'auto'],
        ['grid-area:1/2/3/4', 'grid-row', '1 / 3'],
        ['grid-row:1/span 2', 'grid-row', '1 / span 2'],
        ['grid-area:a/b', 'grid-row', 'a'],
        ['', 'grid-row', 'auto'],
        ['grid-area:1/2/3/4', 'grid-column', '2 / 4'],
        ['grid-column:3', 'grid-column', '3'],
        ['grid-area:a/b', 'grid-column', 'b'],
        ['grid-template:"a b" 20px "c d" 30px / 40px 50px', 'grid-template', '"a b" 20px "c d" 30px / 40px 50px'],
        ['grid-template-areas:"a" "b";grid-template-rows:10px 20px', 'grid-template', '"a" 10px "b" 20px'],
        ['grid-template-areas:"a b";grid-template-rows:10px 20px', 'grid-template', ''],
        ['', 'grid-template', 'none'],
        ['', 'grid', 'none / none / none / row / auto / auto'],
        ['columns:2 100px', 'columns', '100px 2'],
        ['columns:3', 'columns', '3'],
        ['columns:40px', 'columns', '40px'],
        ['', 'columns', 'auto'],
        ['column-rule:3px dotted green', 'column-rule', '3px dotted rgb(0, 128, 0)'],
        ['column-rule:0px solid red', 'column-rule', '0px solid rgb(255, 0, 0)'],
        ['', 'column-rule', '3px rgb(0, 0, 0)'],
        ['list-style:square inside', 'list-style', 'inside none square'],
        ['list-style:none', 'list-style', 'outside none none'],
        ['', 'list-style', 'outside none disc'],
        ['border-inline:1px solid red', 'border-inline', '1px solid rgb(255, 0, 0)'],
        ['border-inline:1px solid red;border-inline-start-color:blue', 'border-inline', ''],
        ['border-inline:1px solid red;border-inline-start-color:blue', 'border-inline-start', '1px solid rgb(0, 0, 255)'],
        ['border-top:1px solid red', 'border-block', ''],
        ['', 'border-block', '0px none rgb(0, 0, 0)'],
        // Shorthands that already serialized from their longhands stay covered.
        ['padding:1px 2px', 'padding', '1px 2px'],
        ['margin:3px', 'margin', '3px'],
        ['inset:1px 2px 3px 4px', 'inset', '1px 2px 3px 4px'],
        ['border:2px dashed red', 'border', '2px dashed rgb(255, 0, 0)'],
        ['border-top:1px solid red', 'border-width', '1px 0px 0px'],
        ['border-top:1px solid red', 'border-color', 'rgb(255, 0, 0) rgb(0, 0, 0) rgb(0, 0, 0)'],
        ['background-position:10px 20%', 'background-position', '10px 20%'],
        ['overflow:hidden auto', 'overflow', 'hidden auto'],
        ['flex-flow:column wrap', 'flex-flow', 'column wrap'],
        ['transition:opacity 1s ease-in .5s', 'transition', 'opacity 1s ease-in 0.5s'],
    ];
    const failures = [];
    for (const [text, property, expected] of cases) {
        const element = document.createElement('div');
        element.style.cssText = text;
        document.body.append(element);
        const actual = getComputedStyle(element).getPropertyValue(property);
        if (actual !== expected) failures.push(`${text || '(initial)'} ${property}: ${JSON.stringify(actual)} != ${JSON.stringify(expected)}`);
        element.remove();
    }
    const element = document.createElement('div');
    element.style.cssText = 'border-radius:18px;gap:4px 8px;flex:1 1 0%';
    document.body.append(element);
    const style = getComputedStyle(element);
    const camel = [style.borderRadius, style.gap, style.flex].join('|');
    if (camel !== '18px|4px 8px|1 1 0%') failures.push(`camelCase accessors: ${camel}`);
    element.remove();
    return failures.length ? failures.join('\n') : 'true';
})()

function drain(queue) {
    while (queue.length > 0) {
        console.log(queue.pop());
    }
}

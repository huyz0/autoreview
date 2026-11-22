function drain(queue: string[]) {
    while (queue.length > 0) {
        console.log(queue.pop());
    }
}

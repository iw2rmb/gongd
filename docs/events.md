# Event Delivery

`gongd` receives raw filesystem events from `notify` and forwards them into an internal queue for async processing.

The filesystem watcher callback must not block. It uses a non-blocking send into the internal queue so shutdown cannot wait on a full queue from inside the OS watcher thread.

If the internal raw event queue is full, the raw watcher event is dropped. This favors daemon responsiveness and clean watcher shutdown over unbounded event buffering.

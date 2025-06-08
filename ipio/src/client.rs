// TODO: potentially create a pool of clients to handle multiple concurrent requests
// otherwise, there's probably no need of server-side synchronization because Linux kernel should already handle that
// we also need to handle splitting or mergin of the iovec buffers as there is a fixed size limit for our IPC

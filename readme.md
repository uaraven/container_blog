# Container Runtime

Toy "container" runtime. This is not really a container per se, but a sandbox build with the same Linux kernel tools that real container runtimes use.

Supports namespaces:
 - PID
 - User
 - Mount
 - Net
 - UTS

Uses Alpine minimal root filesystem as a basis for the overlay filesystem for the sandbox.


## Blog

This source code accompanies the series of my blog posts about writing a container runtime in Rust.

See [blog](https://voronin.cc/posts/container/index.html)

The source code is tagged with tags named "a1", "a2", etc corresponding to the blog posts.a

- [a1](https://github.com/uaraven/container_blog/commits/a1) - [Project and PID namespace](https://voronin.cc/posts/container-rust-project-pid-ns/index.html)
- [a2](https://github.com/uaraven/container_blog/commits/a2) - [User namespace](https://voronin.cc/posts/container-userns/index.html)
- [a3](https://github.com/uaraven/container_blog/commits/a3) - [Mount namespace](https://voronin.cc/posts/container-mount/index.html)

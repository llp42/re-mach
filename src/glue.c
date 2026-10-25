// SPDX-License-Identifier: BSD-2-Clause AND BSD-3-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: sys/kern/init_main.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 1995 Terrence R. Lambert
// SPDX-FileCopyrightText: 1982, 1986, 1989, 1991, 1992, 1993 The Regents of the University of California.
//
// Derived from DragonFly BSD
// Upstream-Path: sys/kern/kern_exec.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1993, David Greenman
//
// Derived from DragonFly BSD
// Upstream-Path: sys/kern/imgact_elf.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2000 David O'Brien
// SPDX-FileCopyrightText: 1995-1996 Søren Schmidt
// SPDX-FileCopyrightText: 1996 Peter Wemm

/*
 * Hooks the Rust kernel crate into the boot sequence, and gives it the
 * process operations it needs to start the init program. Each operation
 * acts on the calling process and returns 0 or an errno.
 */

#include <sys/param.h>
#include <sys/systm.h>
#include <sys/kernel.h>
#include <sys/proc.h>
#include <sys/filedesc.h>
#include <sys/mount.h>
#include <sys/vnode.h>
#include <sys/namecache.h>
#include <sys/imgact.h>
#include <sys/reg.h>
#include <sys/usched.h>
#include <sys/uio.h>

#include <vm/vm.h>
#include <vm/vm_param.h>
#include <sys/lock.h>
#include <vm/pmap.h>
#include <vm/vm_map.h>

void	rust_init(void *);
void	rust_start_init(void *, struct trapframe *);
int	glue_init_root(void);
int	glue_init_vmspace(void);
int	glue_user_map(u_long, u_long);
int	glue_user_protect(u_long, u_long, int);
void	glue_user_enter(u_long, u_long);
u_long	glue_user_stack_top(void);
int	glue_uio_read(struct uio *, char *, size_t);

/* After the console is up, before root is mounted. */
SYSINIT(rust_init, SI_SUB_DRIVERS, SI_ORDER_ANY, rust_init, NULL);

/*
 * Runs after init's process is created and before it is first scheduled,
 * so that it starts in rust_start_init instead of executing /sbin/init.
 */
static void
init_hook(void *dummy __unused)
{
	cpu_set_fork_handler(ONLY_LWP_IN_PROC(initproc), rust_start_init,
	    NULL);
}
SYSINIT(rust_start_init, SI_SUB_DRIVERS, SI_ORDER_ANY, init_hook, NULL);

/*
 * Makes the mounted root the root and current directory. The root is
 * mounted read-only, which forbids opening even device nodes for writing; a
 * devfs root, which has no backing store and cannot be remounted, is made
 * writable, as a devfs mounted on /dev would be.
 */
int
glue_init_root(void)
{
	struct proc *p = curproc;
	struct mount *mp;
	struct vnode *vp;
	int error;

	mp = mountlist_boot_getfirst();
	if (mp == NULL)
		return (ENOENT);
	if (strcmp(mp->mnt_stat.f_fstypename, "devfs") == 0)
		mp->mnt_flag &= ~MNT_RDONLY;
	error = VFS_ROOT(mp, &vp);
	if (error)
		return (error);
	if (mp->mnt_ncmountpt.ncp == NULL) {
		cache_allocroot(&mp->mnt_ncmountpt, mp, vp);
		cache_unlock(&mp->mnt_ncmountpt);	/* leave ref intact */
	}
	p->p_fd->fd_cdir = vp;
	vref(p->p_fd->fd_cdir);
	p->p_fd->fd_rdir = vp;
	vref(p->p_fd->fd_rdir);
	vfs_cache_setroot(vp, cache_hold(&mp->mnt_ncmountpt));
	vn_unlock(vp);				/* leave ref intact */
	cache_copy(&mp->mnt_ncmountpt, &p->p_fd->fd_ncdir);
	cache_copy(&mp->mnt_ncmountpt, &p->p_fd->fd_nrdir);
	return (0);
}

/* Replaces the address space with an empty one holding only a stack. */
int
glue_init_vmspace(void)
{
	struct proc *p = curproc;
	struct image_params image = { .proc = p };
	int error;

	lwkt_gettoken(&p->p_token);
	error = exec_new_vmspace(&image, NULL);
	if (image.vmspace_destroyed & 2)
		p->p_flags &= ~P_INEXEC;
	lwkt_reltoken(&p->p_token);
	return (error);
}

/* Maps zeroed, writable memory at exactly [addr, addr + len). */
int
glue_user_map(u_long addr, u_long len)
{
	struct proc *p = curproc;
	vm_offset_t start = addr;
	int rv;

	lwkt_gettoken(&p->p_token);
	rv = vm_map_find(&p->p_vmspace->vm_map, NULL, NULL, 0, &start, len,
	    PAGE_SIZE, FALSE, VM_MAPTYPE_NORMAL, VM_SUBSYS_IMGACT,
	    VM_PROT_ALL, VM_PROT_ALL, 0);
	lwkt_reltoken(&p->p_token);
	return (rv == KERN_SUCCESS ? 0 : EINVAL);
}

/* Sets the protection of [addr, addr + len) to the VM_PROT_* bits in prot. */
int
glue_user_protect(u_long addr, u_long len, int prot)
{
	struct proc *p = curproc;
	int rv;

	lwkt_gettoken(&p->p_token);
	rv = vm_map_protect(&p->p_vmspace->vm_map, addr, addr + len, prot,
	    FALSE);
	lwkt_reltoken(&p->p_token);
	return (rv == KERN_SUCCESS ? 0 : EINVAL);
}

/*
 * Sets the registers for a fresh program at entry with the given stack, so
 * that returning from the fork handler enters user mode there.
 */
void
glue_user_enter(u_long entry, u_long stack)
{
	struct lwp *lp = curthread->td_lwp;

	exec_setregs(entry, stack, 0);
	lp->lwp_proc->p_usched->acquire_curproc(lp);
}

/* The top of the user stack, above every user mapping. */
u_long
glue_user_stack_top(void)
{
	return (USRSTACK);
}

/*
 * Moves up to len bytes that uio still describes into buf; returns the
 * count, 0 when none remain, or a negated errno.
 */
int
glue_uio_read(struct uio *uio, char *buf, size_t len)
{
	size_t n = MIN(len, uio->uio_resid);
	int error;

	if (n == 0)
		return (0);
	error = uiomove(buf, n, uio);
	return (error ? -error : (int)n);
}

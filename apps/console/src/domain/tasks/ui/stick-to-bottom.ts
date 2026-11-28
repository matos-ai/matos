import { useCallback, useEffect, useRef, useState } from 'react'
import { isNearBottom } from '#/domain/tasks/ui/agent-turn-model'

function prefersReducedMotion(): boolean {
	return (
		typeof window.matchMedia === 'function' &&
		window.matchMedia('(prefers-reduced-motion: reduce)').matches
	)
}

export function useStickToBottom(enabled: boolean) {
	const [scroller, setScroller] = useState<HTMLElement | null>(null)
	const [content, setContent] = useState<HTMLElement | null>(null)
	const [atBottom, setAtBottom] = useState(true)
	const pinned = useRef(true)
	const following = useRef(enabled)

	useEffect(() => {
		following.current = enabled
	}, [enabled])

	useEffect(() => {
		if (!scroller) return
		const measure = () => {
			const near = isNearBottom(scroller)
			pinned.current = near
			setAtBottom(near)
		}
		measure()
		scroller.addEventListener('scroll', measure, { passive: true })
		const observer =
			typeof ResizeObserver === 'undefined' || !content
				? null
				: new ResizeObserver(() => {
						if (following.current && pinned.current) {
							scroller.scrollTop = scroller.scrollHeight
						}
					})
		if (content) observer?.observe(content)
		return () => {
			scroller.removeEventListener('scroll', measure)
			observer?.disconnect()
		}
	}, [scroller, content])

	const scrollToBottom = useCallback(() => {
		if (!scroller) return
		if (typeof scroller.scrollTo === 'function' && !prefersReducedMotion()) {
			scroller.scrollTo({ top: scroller.scrollHeight, behavior: 'smooth' })
		} else {
			scroller.scrollTop = scroller.scrollHeight
		}
	}, [scroller])

	return {
		scrollRef: setScroller,
		contentRef: setContent,
		atBottom,
		scrollToBottom,
	}
}

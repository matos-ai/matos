import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { useStickToBottom } from '#/domain/tasks/ui/stick-to-bottom'

afterEach(cleanup)

function Probe({ enabled }: { enabled: boolean }) {
	const follow = useStickToBottom(enabled)
	return (
		<div ref={follow.scrollRef} data-testid="scroller">
			<div ref={follow.contentRef} />
			<output>{follow.atBottom ? 'bas' : 'haut'}</output>
			<button type="button" onClick={follow.scrollToBottom}>
				go
			</button>
		</div>
	)
}

function metrics(node: HTMLElement, values: Record<string, number>) {
	for (const [key, value] of Object.entries(values)) {
		Object.defineProperty(node, key, {
			configurable: true,
			value,
			writable: true,
		})
	}
}

describe('useStickToBottom', () => {
	it('tracks whether the reader is at the bottom and jumps back down', () => {
		render(<Probe enabled />)
		const scroller = screen.getByTestId('scroller')
		expect(screen.getByRole('status').textContent).toBe('bas')
		metrics(scroller, { scrollHeight: 1000, clientHeight: 100, scrollTop: 100 })
		act(() => {
			fireEvent.scroll(scroller)
		})
		expect(screen.getByRole('status').textContent).toBe('haut')
		fireEvent.click(screen.getByRole('button', { name: 'go' }))
		expect(scroller.scrollTop).toBe(1000)
		act(() => {
			fireEvent.scroll(scroller)
		})
		expect(screen.getByRole('status').textContent).toBe('bas')
	})
})

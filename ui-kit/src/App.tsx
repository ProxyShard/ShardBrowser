import { useState } from 'react'
import {
  Alert,
  AlertModal,
  Badge,
  Breadcrumb,
  Button,
  Checkbox,
  DialogModal,
  Input,
  Modal,
  Pagination,
  ProgressBar,
  Radio,
  SegmentControl,
  Select,
  Slider,
  Switch,
  Tabs,
  Tag,
  Textarea,
  Tooltip,
} from '@/components'
import type { AlertModalStatus, BadgeColor, ButtonMode, ButtonVariant } from '@/components'
import ThemeToggle from '@/theme/ThemeToggle'
import { ICON_STROKE, InfoIcon, SearchIcon } from '@/lib/icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { Link01Icon, ArrowRight01Icon } from '@hugeicons/core-free-icons'
import Section, { Row } from './demo/Section'
import IconsShowcase from './demo/IconsShowcase'
import CountryImage from './components/CountryImage'
import { ALL_COUNTRIES } from './lib/allCountries'

const buttonModes: ButtonMode[] = ['filled', 'stroke', 'lighter', 'ghost']
const buttonVariants: ButtonVariant[] = ['primary', 'neutral', 'error']
const badgeColors: BadgeColor[] = ['success', 'warning', 'error', 'primary', 'information', 'gray']

function Arrow() {
  return <HugeiconsIcon icon={ArrowRight01Icon} strokeWidth={ICON_STROKE} className="size-4" aria-hidden />
}

export default function App() {
  const [checkbox, setCheckbox] = useState(true)
  const [radio, setRadio] = useState('a')
  const [toggle, setToggle] = useState(true)
  const [select, setSelect] = useState('residential')
  const [slider, setSlider] = useState(48)
  const [tab] = useState('overview')
  const [segment, setSegment] = useState('day')
  const [tags, setTags] = useState(['Residential', 'US', 'Rotating'])
  const [modal, setModal] = useState(false)
  const [email, setEmail] = useState('')
  const [alertModal, setAlertModal] = useState<AlertModalStatus | null>(null)
  const [dontShow, setDontShow] = useState(false)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [referral, setReferral] = useState('your-brand-2026')
  const [pageNum, setPageNum] = useState(2)

  const alertModalCopy: Record<AlertModalStatus, { title: string; description: string; confirm: string }> = {
    feature: { title: 'New feature available', description: 'Rotating residential sessions are now live for your account.', confirm: 'Explore' },
    information: { title: 'Heads up', description: 'Your plan renews in 3 days. Review your usage before then.', confirm: 'Got it' },
    success: { title: 'Payment successful', description: 'Your subscription has been renewed. Thanks!', confirm: 'Continue' },
    warning: { title: 'Usage nearing limit', description: 'You have used 82% of your monthly bandwidth.', confirm: 'Add traffic' },
    error: { title: 'Delete proxy?', description: 'This action cannot be undone.', confirm: 'Delete' },
  }

  return (
    <div className="min-h-screen bg-bg-weak-50 px-4 py-8 md:px-8">
      <div className="mx-auto flex max-w-5xl flex-col gap-6">
        <header className="flex flex-col gap-4 rounded-surface border border-stroke-soft-200 bg-bg-white-0 p-6 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex flex-col gap-1">
            <h1 className="text-page-title text-text-strong-950">ShardBrowser UI Kit</h1>
            <p className="text-body text-text-sub-600">
              React + Tailwind CSS v4, built from the Proxy Shard Figma, light &amp; dark aware
            </p>
          </div>
          <ThemeToggle />
        </header>

        <Section title="Buttons" description="3 intents × 4 modes × 4 sizes, with icons, loading & disabled.">
          {buttonModes.map((mode) => (
            <Row key={mode} label={mode}>
              {buttonVariants.map((variant) => (
                <Button key={variant} variant={variant} mode={mode}>Button</Button>
              ))}
              <Button mode={mode} rightIcon={<Arrow />}>Icon</Button>
              <Button mode={mode} onlyIcon leftIcon={<Arrow />} aria-label="Go" />
              <Button mode={mode} isLoading>Loading</Button>
            </Row>
          ))}
          <Row label="sizes">
            <Button size="medium">Medium</Button>
            <Button size="small">Small</Button>
            <Button size="xsmall">X-Small</Button>
            <Button size="2xsmall">2X-Small</Button>
            <Button size="3xsmall">3X-Small</Button>
            <Button fullRadius rightIcon={<Arrow />}>Pill</Button>
            <Button disabled>Disabled</Button>
          </Row>
        </Section>

        <Section title="Badges & Tags" description="Status badges and removable tags.">
          <Row label="status badges">
            {badgeColors.map((c) => (
              <Badge key={c} color={c} dot>{c}</Badge>
            ))}
            {badgeColors.map((c) => (
              <Badge key={`${c}-stroke`} color={c} variant="stroke">{c}</Badge>
            ))}
          </Row>
          <Row label="tags">
            {tags.map((t) => (
              <Tag key={t} onRemove={() => setTags((prev) => prev.filter((x) => x !== t))}>{t}</Tag>
            ))}
            <Tag variant="gray">Read only</Tag>
          </Row>
        </Section>

        <Section title="Alerts" description="Inline feedback in every status and variant.">
          <Alert status="information" variant="lighter" title="Heads up" onClose={() => { }}>
            A new residential pool is now available across 50+ regions.
          </Alert>
          <Alert status="success" variant="light" title="Payment received">
            Your subscription has been renewed successfully.
          </Alert>
          <Alert status="warning" variant="stroke" title="Usage nearing limit">
            You have used 82% of your monthly bandwidth.
          </Alert>
          <Alert status="error" variant="filled" title="Connection failed" onClose={() => { }}>
            We couldn't reach the gateway. Please try again.
          </Alert>
        </Section>

        <Section title="Inputs" description="Text fields, textarea and select.">
          <div className="grid w-full gap-4 sm:grid-cols-2">
            <Input label="Email" placeholder="you@example.com" leftIcon={<SearchIcon className="size-4" />} value={email} onChange={(e) => setEmail(e.target.value)} hint="We'll never share it." />
            <Input label="With error" placeholder="Invalid" error="This field is required" defaultValue="bad@" />
            <Select
              label="Product"
              isSearchable
              value={select}
              onChange={setSelect}
              options={[
                { label: 'Residential', value: 'resi' },
                { label: 'Datacenter', value: 'dc' },
                { label: 'Mobile', value: 'md' },
                { label: 'ISP', value: 'is' },
                { label: 'United Kingdom', value: 'gb' }
              ]}
            />
            <Input label="Disabled" placeholder="Disabled" disabled />
          </div>
          <Textarea label="Notes" placeholder="Write something…" showCount maxLength={120} />
        </Section>

        <Section title="Selection controls" description="Checkbox, radio and switch.">
          <Row label="checkbox">
            <Checkbox label="Checked" checked={checkbox} onChange={(e) => setCheckbox(e.target.checked)} />
            <Checkbox label="Indeterminate" indeterminate onChange={() => { }} />
            <Checkbox label="Disabled" disabled />
          </Row>
          <Row label="radio">
            <Radio name="demo" label="Option A" checked={radio === 'a'} onChange={() => setRadio('a')} />
            <Radio name="demo" label="Option B" checked={radio === 'b'} onChange={() => setRadio('b')} />
            <Radio name="demo" label="Disabled" disabled />
          </Row>
          <Row label="switch">
            <Switch label="Enabled" checked={toggle} onChange={setToggle} />
            <Switch label="Disabled" disabled />
          </Row>
        </Section>

        <Section title="Navigation" description="Tabs, segment control and breadcrumbs.">
          <Tabs
            value={tab}
            items={[
              { label: 'Overview', value: 'overview', url: '#' },
              { label: 'Proxies', value: 'proxies', url: '#' },
              { label: 'Billing', value: 'billing', url: '#' },
            ]}
          />
          <SegmentControl
            value={segment}
            onChange={setSegment}
            items={[
              { label: 'Day', value: 'day' },
              { label: 'Week', value: 'week' },
              { label: 'Month', value: 'month' },
            ]}
            className="max-w-sm"
          />
          <Breadcrumb
            items={[
              { label: 'Dashboard', href: '#' },
              { label: 'Proxies', href: '#' },
              { label: 'Residential' },
            ]}
          />
        </Section>

        <Section title="Feedback" description="Progress, slider, tooltip and modal.">
          <ProgressBar value={slider} showValue />
          <Slider label="Bandwidth" valueLabel={`${slider} GB`} value={slider} onChange={setSlider} />
          <Row label="tooltip & modal">
            <Tooltip content="More information">
              <Button mode="stroke" variant="neutral" leftIcon={<InfoIcon className="size-4" />}>Hover me</Button>
            </Tooltip>
            <Button onClick={() => setModal(true)}>Open modal</Button>
          </Row>
          <Row label="alert modal (icon variations + don't-show-again)">
            {(['feature', 'information', 'success', 'warning', 'error'] as AlertModalStatus[]).map((s) => (
              <Button key={s} mode="lighter" variant={s === 'error' ? 'error' : 'neutral'} onClick={() => setAlertModal(s)}>
                {s}
              </Button>
            ))}
          </Row>
          <Row label="dialog modal (header icon + content + actions)">
            <Button mode="stroke" variant="neutral" onClick={() => setDialogOpen(true)}>Edit referral link</Button>
          </Row>
        </Section>

        <Section title="Pagination" description="URL-driven (?page=N); page 1 omits the param. Here wired to local state.">
          <Pagination page={pageNum} totalPages={16} asLinks={false} onPageChange={setPageNum} />
        </Section>

        <IconsShowcase />

        <Section title="Country Images" description="Country images for all countries.">
          <div className="grid grid-cols-6 gap-4">
            {ALL_COUNTRIES.map((country) => (
              <CountryImage key={country.value} countryCode={country.value} />
            ))}
          </div>
        </Section>
        <footer className="pb-6 text-center text-secondary text-text-soft-400">
          ShardBrowser UI Kit — {badgeColors.length} badge colors, Inter type scale, light &amp; dark.
        </footer>
      </div>

      <Modal
        open={modal}
        onClose={() => setModal(false)}
        title="Delete proxy?"
        description="This action cannot be undone."
        footer={
          <>
            <Button mode="stroke" variant="neutral" onClick={() => setModal(false)}>Cancel</Button>
            <Button variant="error" onClick={() => setModal(false)}>Delete</Button>
          </>
        }
      >
        The selected residential proxy and its rotation settings will be permanently removed.
      </Modal>

      {alertModal && (
        <AlertModal
          open={alertModal !== null}
          onClose={() => setAlertModal(null)}
          status={alertModal}
          title={alertModalCopy[alertModal].title}
          description={alertModalCopy[alertModal].description}
          confirmLabel={alertModalCopy[alertModal].confirm}
          onConfirm={() => setAlertModal(null)}
          showDontShowAgain
          dontShowAgain={dontShow}
          onDontShowAgainChange={setDontShow}
        />
      )}

      <DialogModal
        open={dialogOpen}
        onClose={() => setDialogOpen(false)}
        icon={<HugeiconsIcon icon={Link01Icon} strokeWidth={ICON_STROKE} className="size-5" />}
        title="Edit referral link"
        subtitle="Personalize your referral link"
        confirmLabel="Apply Changes"
        onConfirm={() => setDialogOpen(false)}
      >
        <div className="flex flex-col gap-4">
          <Input label="Referral code" value={referral} onChange={(e) => setReferral(e.target.value)} />
          <Select
            label="Referral code"
            value={referral}
            onChange={setReferral}
            options={[
              { label: 'Referral code', value: 'referral' },
              { label: 'Referral code', value: 'referral1' },
              { label: 'Referral code', value: 'referral2' },
              { label: 'Referral code', value: 'referral3' },
            ]}
          />
          <p className="text-body text-text-sub-600">
            https://shardx.com?ref=
            <span className="text-ui text-text-strong-950">{referral}</span>
          </p>
        </div>
      </DialogModal>
    </div>
  )
}

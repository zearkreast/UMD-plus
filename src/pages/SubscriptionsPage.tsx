import { SubscriptionsList } from '@/components/subscriptions/SubscriptionsList';

export function SubscriptionsPage({ onGoHome }: { onGoHome: () => void }) {
  return (
    <div className="mx-auto w-full max-w-[760px] px-6 pb-12">
      <SubscriptionsList onGoHome={onGoHome} />
    </div>
  );
}
